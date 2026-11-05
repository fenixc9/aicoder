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
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
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
                Message::text(MessageRole::Assistant, "final")
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
struct Startup {
    calls: Arc<AtomicUsize>,
    mode: bool,
}
#[async_trait]
impl BeforeStartHook for Startup {
    async fn before_start(
        &self,
        input: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let selected = if self.mode { "codemode" } else { "write_file" };
        ensure!(
            input.available_tools.iter().any(|t| t.name == selected),
            "missing admitted root"
        );
        if self.mode {
            ensure!(
                input.available_tools.len() == 1,
                "only mode leaked leaf declarations"
            );
        }
        Ok(Some(BeforeStartAction {
            system_prefix: Some(vec![Message::text(
                MessageRole::System,
                "PRIVATE_STARTUP_RULES",
            )]),
            tools: Some(vec![selected.into()]),
            append: vec![
                BoundaryEntry::Context {
                    kind: "start".into(),
                    text: "STARTUP_FACT <script>inert</script>".into(),
                },
                BoundaryEntry::Data {
                    key: "audit".into(),
                    value: json!("INERT_STARTUP_DATA"),
                },
            ],
        }))
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
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Extension::new("example.startup", "1")?;
    e.before_start_hook(
        HookSpec::new("prepare"),
        Arc::new(Startup {
            calls: calls.clone(),
            mode,
        }),
    )?;
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
    let result = agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "literal original input",
            events.clone(),
        )
        .await?;
    ensure!(
        calls.load(Ordering::SeqCst) == 1,
        "startup repeated during preflight/tool round"
    );
    ensure!(
        root.path().join("result.txt").exists() == allow,
        "startup bypassed leaf approval"
    );
    if allow {
        ensure!(
            std::fs::read_to_string(root.path().join("result.txt"))? == "approved",
            "owned file operation changed"
        );
    }
    let requests = provider.requests.lock().unwrap();
    ensure!(
        requests.len() == 2 && result.execution_result.usage.total_tokens == Some(14),
        "request count/usage incorrect"
    );
    for request in requests.iter() {
        ensure!(
            request.messages[0].text_content() == "PRIVATE_STARTUP_RULES",
            "run prefix changed between rounds"
        );
        ensure!(
            request.tools.len() == 1
                && request.tools[0].name == if mode { "codemode" } else { "write_file" },
            "startup root selection lost"
        );
        ensure!(
            request
                .messages
                .iter()
                .any(|m| m.text_content().contains("STARTUP_FACT")),
            "startup context missing"
        );
    }
    ensure!(
        !serde_json::to_string(&*requests)?.contains("INERT_STARTUP_DATA"),
        "inert startup data became instructions"
    );
    let saved = repo.open(&session.metadata().id)?;
    ensure!(saved.unfinished_turn().is_none(), "turn not closed");
    let users = saved
        .messages()
        .iter()
        .filter(|m| m.message.role == MessageRole::User)
        .map(|m| m.message.text_content())
        .collect::<Vec<_>>();
    ensure!(
        users == ["literal original input"],
        "startup changed/fabricated user input"
    );
    let native = saved
        .entries()
        .iter()
        .filter(|e| matches!(e.data, SessionEntryData::NativeContext { .. }))
        .count();
    ensure!(native == 1, "startup context was duplicated");
    let raw_history = serde_json::to_string(saved.entries())?;
    ensure!(
        !raw_history.contains("PRIVATE_STARTUP_RULES")
            && raw_history.contains("INERT_STARTUP_DATA"),
        "private prefix persisted or receipt missing"
    );
    let raw = events.0.lock().unwrap();
    let mut validator = EventTraceValidator::new();
    for e in raw.iter() {
        validator.observe(e)?;
    }
    validator.finish()?;
    ensure!(
        raw.iter()
            .filter(|e| matches!(
                e.event,
                AgentRawEvent::LifecycleBoundaryCommitted {
                    boundary: BoundaryKind::BeforeStart,
                    ..
                }
            ))
            .count()
            == 1,
        "startup notification missing/duplicated"
    );
    ensure!(
        !serde_json::to_string(&*raw)?.contains("PRIVATE_STARTUP_RULES"),
        "private prefix leaked into event body"
    );
    let archive = JsonlSessionRepository::with_limits(
        runtime.session_dir()?,
        SessionLoadLimits {
            max_snapshot_bytes: 1024,
            max_window_messages: 3,
            ..Default::default()
        },
    )?;
    let run = archive.open_run(&session.metadata().id)?;
    ensure!(
        matches!(run, RunSession::Window(_)),
        "archive fixture did not use window"
    );
    let (_, context) = run.source_and_context()?;
    ensure!(
        !serde_json::to_string(&context)?.contains("STARTUP_FACT"),
        "old descriptor leaked into bounded model window"
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
        html.contains("STARTUP_FACT") && !html.contains("<script>inert</script>"),
        "export omitted/activated startup context"
    );
    Ok(
        json!({"code_mode":mode,"approved":allow,"startup_calls":calls.load(Ordering::SeqCst),"model_requests":requests.len(),"usage":result.execution_result.usage.total_tokens,"native_context_messages":native,"original_user_count":users.len(),"inert_data_not_prompted":true,"private_prefix_not_persisted":true,"notifications_valid":true,"bounded_archive_verified":true,"export_inert":true,"network_requests":0}),
    )
}
