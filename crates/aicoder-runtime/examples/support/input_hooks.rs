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
struct Raw {
    handled: bool,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl InputHook for Raw {
    async fn input(
        &self,
        input: InputHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<InputAction> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ensure!(
            input.delivery == InputDelivery::Initial && input.source == InputSource::Sdk,
            "wrong source/delivery"
        );
        ensure!(
            input.input.message().text_content() == "/rewrite RAW_PRIVATE_INPUT",
            "raw hook ran after expansion"
        );
        if self.handled {
            Ok(InputAction::Handled)
        } else {
            Ok(InputAction::Transform(
                input.input.clone().with_prompt_text("/task translated"),
            ))
        }
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(event.clone());
    }
}
pub async fn run(mode: bool, allow: bool, handled: bool) -> Result<Value> {
    ensure!(
        !mode || cfg!(feature = "code-mode"),
        "enable code-mode for this variant"
    );
    let root = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    std::fs::create_dir_all(root.path().join(".aicoder/prompts"))?;
    std::fs::write(
        root.path().join(".aicoder/prompts/task.md"),
        "TEMPLATE_FACT $1 <script>inert</script>",
    )?;
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
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Extension::new("example.input", "1")?;
    e.input_hook(
        HookSpec::new("raw"),
        Arc::new(Raw {
            handled,
            calls: calls.clone(),
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
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "/rewrite RAW_PRIVATE_INPUT",
            events.clone(),
            TurnExecutionContext::new(),
        )
        .await;
    ensure!(
        calls.load(Ordering::SeqCst) == 1,
        "raw input was processed more than once"
    );
    let saved = repo.open(&session.metadata().id)?;
    let raw = events.0.lock().unwrap();
    let mut validator = EventTraceValidator::new();
    for event in raw.iter() {
        validator.observe(event)?;
    }
    validator.finish()?;
    ensure!(
        !serde_json::to_string(&*raw)?.contains("RAW_PRIVATE_INPUT"),
        "raw provenance leaked into event wire"
    );
    let requests = provider.requests.lock().unwrap();
    if handled {
        ensure!(
            matches!(&outcome,AgentTurnOutcome::Stopped(turn)if matches!(turn.stop,RunStop::InputHandled(_))),
            "handled input was misclassified"
        );
        ensure!(
            requests.is_empty()
                && saved.entries().is_empty()
                && !root.path().join("result.txt").exists(),
            "handled input fabricated a turn or executed an operation"
        );
        return Ok(
            json!({"code_mode":mode,"approved":allow,"handled":true,"hook_calls":1,"model_requests":0,"original_user_count":0,"network_requests":0,"notifications_valid":true}),
        );
    }
    ensure!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "transformed input did not complete"
    );
    ensure!(
        requests.len() == 2 && outcome.progress().usage.total_tokens == Some(14),
        "model usage/count changed"
    );
    for request in requests.iter() {
        let text = serde_json::to_string(&request.messages)?;
        ensure!(
            text.contains("TEMPLATE_FACT translated") && !text.contains("RAW_PRIVATE_INPUT"),
            "request did not use the expanded effective input"
        );
    }
    ensure!(
        root.path().join("result.txt").exists() == allow,
        "raw input hooks bypassed tool approval"
    );
    let users = saved
        .messages()
        .iter()
        .filter(|m| m.message.role == MessageRole::User)
        .map(|m| m.message.text_content())
        .collect::<Vec<_>>();
    ensure!(
        users.len() == 1 && users[0].contains("TEMPLATE_FACT translated"),
        "transformed canonical input changed"
    );
    ensure!(
        !serde_json::to_string(saved.entries())?.contains("RAW_PRIVATE_INPUT"),
        "raw original leaked into persisted metadata"
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
        "fixture did not use archive window"
    );
    let (_, context) = run.source_and_context()?;
    ensure!(
        !serde_json::to_string(&context)?.contains("RAW_PRIVATE_INPUT"),
        "inert provenance became archive prompt instructions"
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
        html.contains("TEMPLATE_FACT") && !html.contains("<script>inert</script>"),
        "effective user input export became active"
    );
    Ok(
        json!({"code_mode":mode,"approved":allow,"handled":false,"hook_calls":1,"model_requests":2,"usage":14,"original_user_count":1,"raw_provenance_not_prompted":true,"notifications_valid":true,"bounded_archive_verified":true,"export_inert":true,"network_requests":0}),
    )
}
