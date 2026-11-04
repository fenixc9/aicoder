use aicoder_core::{extensions::*, model::*, session::*, tools::*, *};
use aicoder_runtime::{code_mode::CodeModeSettings, resources::ResourceSettings, *};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Normalize;
#[async_trait]
impl ToolCallHook for Normalize {
    async fn before_call(
        &self,
        input: ToolCallHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<ToolCallEdit> {
        if input.call.name != "write_file" {
            return Ok(ToolCallEdit::Continue);
        }
        let mut args = input.call.arguments.clone();
        args["path"] = json!("actual.txt");
        args["content"] = json!("NORMALIZED\n");
        Ok(ToolCallEdit::Arguments(args))
    }
}
struct Redact;
#[async_trait]
impl ToolResultHook for Redact {
    async fn after_tool(
        &self,
        input: ToolResultHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<ToolResultEdit>> {
        if input.call.name != "read_file" || input.envelope["ok"] != true {
            return Ok(None);
        }
        let mut payload = input.envelope["output"].clone();
        payload["content"] = json!("[redacted]");
        Ok(Some(ToolResultEdit {
            is_error: false,
            payload,
            artifacts: vec![],
        }))
    }
}
struct Provider {
    code_mode: bool,
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
        let first = request.messages.last().unwrap().role == MessageRole::User;
        self.requests.lock().unwrap().push(request.clone());
        let calls = if self.code_mode {
            vec![ModelToolCall {
                id: "program".into(),
                name: "codemode".into(),
                arguments: json!({"code":"await tools.write_file({path:'requested.txt',content:'input'});text((await tools.read_file({path:'secret.txt'})).content);"}),
            }]
        } else {
            vec![
                ModelToolCall {
                    id: "write".into(),
                    name: "write_file".into(),
                    arguments: json!({"path":"requested.txt","content":"input"}),
                },
                ModelToolCall {
                    id: "read".into(),
                    name: "read_file".into(),
                    arguments: json!({"path":"secret.txt"}),
                },
            ]
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: if first {
                Message {
                    role: MessageRole::Assistant,
                    content: calls
                        .into_iter()
                        .map(|call| ContentBlock::ToolCall { call })
                        .collect(),
                }
            } else {
                Message::text(MessageRole::Assistant, "done")
            },
            stop_reason: if first {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Approval {
    allow: bool,
    seen: Arc<Mutex<Vec<ToolInvocation>>>,
}
#[async_trait]
impl ApprovalHandler for Approval {
    async fn approve(&self, invocation: &ToolInvocation) -> Result<bool> {
        self.seen.lock().unwrap().push(invocation.clone());
        Ok(self.allow)
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<Value>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        if let AgentRawEvent::ToolExecutionEnded { .. } = &event.event {
            self.0
                .lock()
                .unwrap()
                .push(serde_json::to_value(event).unwrap());
        }
    }
}

/// Actual runtime tools/JSONL sessions, no HTTP or model credentials. Returns verified receipts.
pub async fn run(code_mode: bool, allow: bool) -> Result<Value> {
    ensure!(
        !code_mode || cfg!(feature = "code-mode"),
        "build with code-mode for the program example"
    );
    let root = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    std::fs::write(root.path().join("secret.txt"), "SECRET_FROM_DISK")?;
    let runtime = Runtime::load_with_environment(
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                code_mode: CodeModeSettings {
                    enabled: Setting::Value(code_mode),
                    only: Setting::Value(code_mode),
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
    let mut extension = Extension::new("example.interception", "1")?;
    extension
        .tool_call_hook(HookSpec::new("normalize"), Arc::new(Normalize))?
        .tool_result_hook(HookSpec::new("redact"), Arc::new(Redact))?;
    let mut extensions = ExtensionRegistry::new();
    extensions.install(extension)?;
    let model = Arc::new(Provider {
        code_mode,
        requests: Mutex::new(vec![]),
    });
    let approvals = Arc::new(Mutex::new(vec![]));
    let events = Arc::new(Events::default());
    let agent = Agent::new(
        runtime
            .executor_builder_with_extensions(model.clone(), Arc::new(extensions.finish()))?
            .approval(Approval {
                allow,
                seen: approvals.clone(),
            })
            .build()?,
        AgentConfig::new("offline"),
    );
    let repo = runtime.sessions()?;
    let session = repo.create(root.path())?;
    let turn = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "Run the offline interception example",
            events.clone(),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    let actual = root.path().join("actual.txt");
    ensure!(
        actual.exists() == allow,
        "approval did not control actual mutation"
    );
    ensure!(
        !root.path().join("requested.txt").exists(),
        "original path was mutated"
    );
    if allow {
        ensure!(
            std::fs::read_to_string(actual)? == "NORMALIZED\n",
            "approved normalized plan was not committed"
        );
    }
    ensure!(
        std::fs::read_to_string(root.path().join("secret.txt"))? == "SECRET_FROM_DISK",
        "read source changed"
    );
    let saved = repo.open(&session.metadata().id)?;
    let encoded = serde_json::to_string(saved.entries())?;
    ensure!(
        !encoded.contains("SECRET_FROM_DISK"),
        "raw delivered secret entered history"
    );
    ensure!(
        !serde_json::to_string(&*events.0.lock().unwrap())?.contains("SECRET_FROM_DISK"),
        "raw secret entered final tool events"
    );
    let requests = model.requests.lock().unwrap();
    ensure!(requests.len() == 2, "unexpected inference count");
    ensure!(
        !serde_json::to_string(&requests[1])?.contains("SECRET_FROM_DISK"),
        "raw delivered secret entered next request"
    );
    if code_mode {
        ensure!(
            requests
                .iter()
                .all(|r| r.tools.len() == 1 && r.tools[0].name == "codemode"),
            "root only exposure changed"
        );
    }
    let approvals = approvals.lock().unwrap();
    if allow {
        let write = approvals
            .iter()
            .find(|v| v.name == "write_file")
            .ok_or_else(|| anyhow::anyhow!("write approval missing"))?;
        ensure!(
            write.arguments["path"] == "actual.txt" && write.arguments["content"] == "NORMALIZED\n",
            "approval saw original arguments"
        );
        ensure!(
            write
                .preview
                .as_ref()
                .is_some_and(|p| p.body.contains("NORMALIZED")),
            "approval saw a stale preview"
        );
    }
    let receipts=saved.entries().iter().filter(|e|matches!(&e.data,SessionEntryData::Extension{namespace,..}if namespace==INTERCEPTION_NAMESPACE)).count();
    let tool_messages = turn
        .execution_result
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .count();
    ensure!(
        tool_messages == if code_mode { 1 } else { 2 },
        "canonical tool indices changed"
    );
    Ok(
        json!({"code_mode":code_mode,"approved":allow,"actual_mutation":actual_exists(root.path()),"original_path_untouched":true,"source_unchanged":true,"raw_output_redacted":true,"approved_owned_plan":allow,"interception_receipts":receipts,"tool_messages":tool_messages,"network_requests":0}),
    )
}
fn actual_exists(root: &std::path::Path) -> bool {
    root.join("actual.txt").exists()
}
