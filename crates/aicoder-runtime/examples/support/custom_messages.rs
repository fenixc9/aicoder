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
struct Startup(Arc<AtomicUsize>);
#[async_trait]
impl BeforeStartHook for Startup {
    async fn before_start(
        &self,
        input: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        let Some(RunTrigger::Custom(custom)) = input.trigger else {
            anyhow::bail!("missing custom startup trigger")
        };
        ensure!(
            custom.custom_type() == "wake" && custom.details().is_some(),
            "startup lost custom metadata"
        );
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Some(BeforeStartAction {
            append: vec![BoundaryEntry::Context {
                kind: "startup".into(),
                text: "STARTUP_CUSTOM_CONTEXT".into(),
            }],
            ..Default::default()
        }))
    }
}
struct RawNever;
#[async_trait]
impl InputHook for RawNever {
    async fn input(&self, _: InputHookInput<'_>, _: ExtensionCancellation) -> Result<InputAction> {
        anyhow::bail!("custom messages entered raw user processing")
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, e: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(e.clone());
    }
}
pub async fn run(mode: bool, allow: bool, record_only: bool) -> Result<Value> {
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
                    enabled: Setting::Value(true),
                    global: Setting::Value(false),
                    parents: Setting::Value(false),
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
    let starts = Arc::new(AtomicUsize::new(0));
    let mut e = Extension::new("custom-fixture", "1")?;
    e.before_start_hook(HookSpec::new("start"), Arc::new(Startup(starts.clone())))?;
    e.input_hook(HookSpec::new("never"), Arc::new(RawNever))?;
    let mut r = ExtensionRegistry::new();
    r.install(e)?;
    let provider = Arc::new(Provider {
        mode,
        requests: Mutex::new(vec![]),
    });
    let builder =
        runtime.executor_builder_with_extensions(provider.clone(), Arc::new(r.finish()))?;
    let builder = if allow {
        builder.approval(AllowAllApproval)
    } else {
        builder.approval(DenyAllApproval)
    };
    let agent = Agent::new(builder.build()?, AgentConfig::new("offline"));
    let repo = runtime.sessions()?;
    let session = repo.create(root.path())?;
    agent
        .append_custom_message(
            &repo,
            &session.metadata().id,
            CustomMessage::text("sender", "note", "OLD_CUSTOM_FACT")
                .unwrap()
                .with_display(false),
            TurnExecutionContext::new(),
        )
        .await?;
    ensure!(
        starts.load(Ordering::SeqCst) == 0 && provider.requests.lock().unwrap().is_empty(),
        "record-only custom message ran an agent"
    );
    if record_only {
        let saved = repo.open(&session.metadata().id)?;
        ensure!(
            saved.entries().len() == 1 && saved.messages()[0].message.role == MessageRole::Custom,
            "record-only custom message fabricated user/turn data"
        );
        return Ok(
            json!({"code_mode":mode,"approved":allow,"record_only":true,"model_requests":0,"custom_messages":1,"user_messages":0,"startup_calls":0,"network_requests":0}),
        );
    }
    let events = Arc::new(Events::default());
    let result = agent
        .run_prompt_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            CustomMessage::text(
                "sender",
                "wake",
                "/NOT_A_TEMPLATE WAKE_CUSTOM_FACT <script>inert</script>",
            )?
            .with_details(json!({"private_details":"INERT_DETAILS"}))?,
            events.clone(),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    ensure!(
        starts.load(Ordering::SeqCst) == 1,
        "custom startup repeated"
    );
    ensure!(
        root.path().join("result.txt").exists() == allow,
        "custom messages bypassed owned approval"
    );
    let requests = provider.requests.lock().unwrap();
    ensure!(
        requests.len() == 2 && result.execution_result.usage.total_tokens == Some(14),
        "usage/request count changed"
    );
    for request in requests.iter() {
        let text = serde_json::to_string(&request.messages)?;
        ensure!(
            text.contains("WAKE_CUSTOM_FACT")
                && text.contains("OLD_CUSTOM_FACT")
                && !text.contains("INERT_DETAILS"),
            "custom provider view lost source or activated details"
        );
        ensure!(
            request
                .messages
                .iter()
                .all(|m| m.role != MessageRole::Custom),
            "canonical custom role leaked into provider protocol"
        );
    }
    let saved = repo.open(&session.metadata().id)?;
    let custom = saved
        .messages()
        .iter()
        .filter(|m| m.message.role == MessageRole::Custom)
        .count();
    ensure!(
        custom == 2
            && saved
                .messages()
                .iter()
                .all(|m| m.message.role != MessageRole::User),
        "custom context became user facts"
    );
    let raw = events.0.lock().unwrap();
    let mut validator = EventTraceValidator::new();
    for event in raw.iter() {
        validator.observe(event)?;
    }
    validator.finish()?;
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
        "fixture did not load archive window"
    );
    let (_, context) = run.source_and_context()?;
    ensure!(
        context.iter().all(|m| m.role != MessageRole::Custom)
            && !serde_json::to_string(&context)?.contains("WAKE_CUSTOM_FACT"),
        "private custom descriptors leaked into materialized window"
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
        html.contains("WAKE_CUSTOM_FACT") && !html.contains("<script>inert</script>"),
        "custom HTML export omitted or activated text"
    );
    Ok(
        json!({"code_mode":mode,"approved":allow,"record_only":false,"model_requests":2,"custom_messages":custom,"user_messages":0,"usage":14,"startup_calls":1,"bounded_archive_verified":true,"export_inert":true,"notifications_valid":true,"network_requests":0}),
    )
}
