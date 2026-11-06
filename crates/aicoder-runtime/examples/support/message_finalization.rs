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
struct Finalize(Arc<AtomicUsize>);
#[async_trait]
impl AssistantMessageHook for Finalize {
    async fn finalize(
        &self,
        input: AssistantMessageInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<Message>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        ensure!(
            input.usage.total_tokens == Some(7),
            "usage not accounted before finalization"
        );
        let mut message = input.message.clone();
        if message.tool_calls().next().is_some() {
            message.content.push(ContentBlock::Text {
                text: "FINALIZED_TOOL_NOTE".into(),
            });
        } else {
            message.content = vec![ContentBlock::Text {
                text: "FINALIZED_FINAL <script>inert</script>".into(),
            }];
        }
        Ok(Some(message))
    }
}
struct GeneralFinalize(Arc<AtomicUsize>);
#[async_trait]
impl MessageHook for GeneralFinalize {
    async fn finalize(
        &self,
        input: MessageHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<Message>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let mut message = input.message.clone();
        match message.role {
            MessageRole::User => message = Message::text(MessageRole::User, "FINALIZED_USER_INPUT"),
            MessageRole::Assistant => {
                message.content.push(ContentBlock::Text {
                    text: " GENERAL_FINALIZATION".into(),
                });
            }
            MessageRole::Tool => {
                if let ContentBlock::ToolResult {
                    content, is_error, ..
                } = &mut message.content[0]
                {
                    if *is_error {
                        content["error"]["message"] = json!("FINALIZED_TOOL_DELIVERY");
                    } else {
                        content["output"] = json!({"delivered":"FINALIZED_TOOL_DELIVERY"});
                    }
                }
            }
            _ => {}
        }
        Ok(Some(message))
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
    let mut e = Extension::new("finalize-fixture", "1")?;
    e.assistant_message_hook(HookSpec::new("edit"), Arc::new(Finalize(calls.clone())))?;
    let general = Arc::new(AtomicUsize::new(0));
    e.message_hook(
        HookSpec::new("general"),
        Arc::new(GeneralFinalize(general.clone())),
    )?;
    let mut r = ExtensionRegistry::new();
    r.install(e)?;
    let p = Arc::new(Provider {
        mode,
        requests: Mutex::new(vec![]),
    });
    let builder = runtime.executor_builder_with_extensions(p.clone(), Arc::new(r.finish()))?;
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
            "finish fixture",
            events.clone(),
        )
        .await?;
    ensure!(
        result
            .execution_result
            .final_message
            .text_content()
            .starts_with("FINALIZED_FINAL")
            && result.execution_result.usage.total_tokens == Some(14),
        "final answer/usage changed"
    );
    ensure!(
        calls.load(Ordering::SeqCst) == 2 && p.requests.lock().unwrap().len() == 2,
        "finalization repeated model requests"
    );
    ensure!(
        root.path().join("result.txt").exists() == allow,
        "finalization bypassed owned tool approval"
    );
    ensure!(
        serde_json::to_string(&p.requests.lock().unwrap()[1].messages)?
            .contains("FINALIZED_TOOL_NOTE"),
        "next request did not use finalized history"
    );
    let saved = repo.open(&session.metadata().id)?;
    ensure!(
        saved
            .messages()
            .iter()
            .filter(|m| m.message.role == MessageRole::Assistant)
            .count()
            == 2,
        "assistant persisted twice"
    );
    ensure!(
        general.load(Ordering::SeqCst) == 4,
        "generic finalizers repeated or missed a fresh message"
    );
    ensure!(
        p.requests.lock().unwrap()[0]
            .messages
            .iter()
            .any(|m| m.text_content() == "FINALIZED_USER_INPUT"),
        "user was not finalized before request"
    );
    ensure!(
        serde_json::to_string(&p.requests.lock().unwrap()[1].messages)?
            .contains("FINALIZED_TOOL_DELIVERY"),
        "model did not receive final tool payload"
    );
    ensure!(
        saved
            .messages()
            .iter()
            .any(|m| m.message.role == MessageRole::User
                && m.message.text_content() == "FINALIZED_USER_INPUT"),
        "canonical user differs"
    );
    let raw = events.0.lock().unwrap();
    let mut validator = EventTraceValidator::new();
    for event in raw.iter() {
        validator.observe(event)?;
    }
    validator.finish()?;
    ensure!(
        raw.iter()
            .filter(|e| matches!(
                e.event,
                AgentRawEvent::AssistantMessageFinalized { changed: true, .. }
            ))
            .count()
            == 2,
        "finalized events missing"
    );
    let archive = JsonlSessionRepository::with_limits(
        runtime.session_dir()?,
        SessionLoadLimits {
            max_snapshot_bytes: 1024,
            max_window_messages: 3,
            ..Default::default()
        },
    )?;
    let window = archive.open_run(&session.metadata().id)?;
    ensure!(
        matches!(window, RunSession::Window(_)),
        "archive not windowed"
    );
    let (_, context) = window.source_and_context()?;
    ensure!(
        context
            .last()
            .is_some_and(|m| m.text_content().contains("FINALIZED_FINAL")),
        "archive lost final canonical text"
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
        html.contains("FINALIZED_FINAL") && !html.contains("<script>inert</script>"),
        "export not inert"
    );
    Ok(
        json!({"code_mode":mode,"approved":allow,"model_requests":2,"hook_calls":2,"generic_hook_calls":4,"usage":14,"canonical_assistants":2,"protected_tools_and_approvals":true,"notifications_valid":true,"bounded_archive_verified":true,"export_inert":true,"network_requests":0}),
    )
}
