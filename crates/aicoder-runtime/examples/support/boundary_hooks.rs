use aicoder_core::{events::*, extensions::*, model::*, session::*, tools::*, *};
use aicoder_runtime::{
    code_mode::CodeModeSettings,
    exports::{self, ExportFormat, ExportLimits},
    resources::ResourceSettings,
    session::{JsonlSessionRepository, SessionLoadLimits},
    *,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
struct Provider {
    mode: bool,
    requests: Mutex<Vec<ModelRequest>>,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.tools = true;
        spec.context_tokens = Some(65536);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let n = self.requests.lock().unwrap().len() + 1;
        self.requests.lock().unwrap().push(request.clone());
        let tools = n == 1;
        let call = if self.mode {
            ModelToolCall {
                id: "program".into(),
                name: "codemode".into(),
                arguments: json!({"code":"await tools.write_file({path:'result.txt',content:'approved'});text('tool complete');"}),
            }
        } else {
            ModelToolCall {
                id: "write".into(),
                name: "write_file".into(),
                arguments: json!({"path":"result.txt","content":"approved"}),
            }
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: if tools {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall { call }],
                }
            } else {
                Message::text(
                    MessageRole::Assistant,
                    if n == 2 { "candidate" } else { "final" },
                )
            },
            stop_reason: if tools {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage {
                input_tokens: Some(5),
                output_tokens: Some(2),
                total_tokens: Some(7),
                cached_input_tokens: Some(0),
                cache_write_tokens: Some(0),
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Lifecycle;
#[async_trait]
impl BoundaryHook for Lifecycle {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        let denied = input
            .history
            .iter()
            .filter_map(Message::tool_result)
            .any(|(_, _, error)| error);
        if input.kind == BoundaryKind::TurnEnd && input.round == 1 {
            ensure!(
                input.statistics.tool_calls > 0 && input.statistics.usage.total_tokens == Some(7),
                "boundary did not observe settled tools/usage"
            );
            return Ok(Some(BoundaryAction {
                append: if denied {
                    vec![BoundaryEntry::Data {
                        key: "denied".into(),
                        value: json!({"not_an_instruction":"INERT_AUDIT_DATA"}),
                    }]
                } else {
                    vec![BoundaryEntry::Context {
                        kind: "tool-note".into(),
                        text: "TOOL_STAGE_CONTEXT".into(),
                    }]
                },
                ..Default::default()
            }));
        }
        if input.kind == BoundaryKind::BeforeSettle && input.round == 2 && !denied {
            ensure!(
                input.can_continue && input.statistics.usage.total_tokens == Some(14),
                "continuation statistics incorrect"
            );
            return Ok(Some(BoundaryAction {
                append: vec![
                    BoundaryEntry::Context {
                        kind: "settle-note".into(),
                        text: "SETTLE_STAGE_CONTEXT".into(),
                    },
                    BoundaryEntry::Data {
                        key: "audit".into(),
                        value: json!("INERT_AUDIT_DATA"),
                    },
                ],
                continue_run: Some(true),
                ..Default::default()
            }));
        }
        if input.kind == BoundaryKind::BeforeSettle && input.round == 3 {
            return Ok(Some(BoundaryAction {
                append: vec![BoundaryEntry::Context {
                    kind: "final-note".into(),
                    text: "FINAL_NOTE <script>inert</script>".into(),
                }],
                continue_run: Some(false),
                ..Default::default()
            }));
        }
        Ok(None)
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(event.clone());
    }
}
pub async fn run(mode: bool, allow: bool) -> Result<Value> {
    ensure!(
        !mode || cfg!(feature = "code-mode"),
        "enable code-mode for this variant"
    );
    let root = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let runtime = Runtime::load_with_environment(
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                code_mode: CodeModeSettings {
                    enabled: Setting::Value(mode),
                    only: Setting::Value(mode),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Value(state.path().join("sessions")),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(std::collections::BTreeMap::new()),
    )?;
    let mut e = Extension::new("example.lifecycle", "1")?;
    e.boundary_hook(HookSpec::new("boundaries"), Arc::new(Lifecycle))?;
    let mut extensions = ExtensionRegistry::new();
    extensions.install(e)?;
    let provider = Arc::new(Provider {
        mode,
        requests: Mutex::new(vec![]),
    });
    let builder = runtime
        .executor_builder_with_extensions(provider.clone(), Arc::new(extensions.finish()))?;
    let builder = if allow {
        builder.approval(AllowAllApproval)
    } else {
        builder.approval(DenyAllApproval)
    };
    let agent = Agent::new(builder.build()?, AgentConfig::new("offline"));
    let repo = runtime.sessions()?;
    let session = repo.create(root.path())?;
    let events = Arc::new(Events::default());
    let turn = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "Run lifecycle fixture",
            events.clone(),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    ensure!(
        root.path().join("result.txt").exists() == allow,
        "lifecycle bypassed approval"
    );
    if allow {
        ensure!(
            std::fs::read_to_string(root.path().join("result.txt"))? == "approved",
            "owned mutation not committed"
        );
    }
    let requests = provider.requests.lock().unwrap();
    ensure!(
        requests.len() == if allow { 3 } else { 2 },
        "unexpected continuation count"
    );
    ensure!(
        turn.execution_result.usage.total_tokens == Some(if allow { 21 } else { 14 }),
        "shared usage incorrect"
    );
    ensure!(
        !serde_json::to_string(&*requests)?.contains("INERT_AUDIT_DATA"),
        "inert boundary metadata became instructions"
    );
    if allow {
        ensure!(
            requests[1]
                .messages
                .iter()
                .any(|m| m.text_content().contains("TOOL_STAGE_CONTEXT"))
                && requests[2]
                    .messages
                    .iter()
                    .any(|m| m.text_content().contains("SETTLE_STAGE_CONTEXT")),
            "committed context not delivered"
        );
        ensure!(
            turn.execution_result.final_message.text_content() == "final",
            "final note replaced model answer"
        );
    }
    let saved = repo.open(&session.metadata().id)?;
    ensure!(
        saved
            .messages()
            .iter()
            .filter(|m| m.message.role == MessageRole::User)
            .count()
            == 1,
        "native context fabricated user input"
    );
    let native = saved
        .entries()
        .iter()
        .filter(|e| matches!(e.data, SessionEntryData::NativeContext { .. }))
        .count();
    ensure!(
        native == if allow { 3 } else { 0 },
        "native context count changed"
    );
    let records=saved.entries().iter().filter(|e|matches!(&e.data,SessionEntryData::Extension{namespace,..}if namespace==LIFECYCLE_NAMESPACE)).count();
    ensure!(
        records == if allow { 4 } else { 2 },
        "boundary records missing"
    );
    let raw = events.0.lock().unwrap();
    let mut validator = EventTraceValidator::new();
    for event in raw.iter() {
        validator.observe(event)?;
    }
    validator.finish()?;
    ensure!(
        raw.iter()
            .filter(|e| matches!(e.event, AgentRawEvent::LifecycleBoundaryCommitted { .. }))
            .count()
            == if allow { 3 } else { 1 },
        "boundary notifications missing"
    );
    ensure!(
        !serde_json::to_string(&*raw)?.contains("INERT_AUDIT_DATA"),
        "inert data leaked into notifications"
    );
    if allow {
        let archive = JsonlSessionRepository::with_limits(
            runtime.session_dir()?,
            SessionLoadLimits {
                max_snapshot_bytes: 1024,
                max_window_messages: 2,
                ..Default::default()
            },
        )?;
        let run = archive.open_run(&session.metadata().id)?;
        ensure!(
            matches!(run, RunSession::Window(_)),
            "archive fixture did not use bounded window"
        );
        let (_, context) = run.source_and_context()?;
        let encoded = serde_json::to_string(&context)?;
        ensure!(
            encoded.contains("FINAL_NOTE")
                && !encoded.contains("TOOL_STAGE_CONTEXT")
                && !encoded.contains("SETTLE_STAGE_CONTEXT"),
            "archive descriptors/old context leaked into model window"
        );
        ensure!(
            run.messages().len() <= 2,
            "native messages bypassed window count"
        );
        let export = exports::prepare(
            &repo,
            root.path(),
            &session.metadata().id,
            ExportFormat::Html,
            ExportLimits::default(),
            &TurnExecutionContext::new(),
        )?;
        let html = std::str::from_utf8(export.bytes())?;
        ensure!(
            html.contains("FINAL_NOTE") && !html.contains("<script>inert</script>"),
            "native export became active markup"
        );
    }
    Ok(
        json!({"code_mode":mode,"approved":allow,"model_requests":requests.len(),"usage":turn.execution_result.usage.total_tokens,"native_context_messages":native,"boundary_records":records,"original_user_count":1,"inert_data_not_prompted":true,"notifications_valid":true,"bounded_archive_verified":allow,"export_inert":allow,"network_requests":0}),
    )
}
