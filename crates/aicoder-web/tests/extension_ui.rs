#![cfg(feature = "server")]
use aicoder_core::{extensions::*, model::*, ui::*, *};
use aicoder_runtime::{
    ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings, resources::ResourceSettings,
};
use aicoder_web::{WebHost, WebOptions};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
const TOKEN: &str = "fixture-ui-token-0123456789abcdef";
struct Model {
    write: bool,
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
}
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        let call = self.write
            && request
                .messages
                .last()
                .is_some_and(|m| m.role == MessageRole::User);
        let message = if call {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "one".into(),
                        name: "write_file".into(),
                        arguments: json!({"path":"result.txt","content":"after"}),
                    },
                }],
            }
        } else {
            Message::text(MessageRole::Assistant, "done")
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if call {
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
struct Review(Arc<AtomicUsize>);
#[async_trait]
impl ExtensionCommand for Review {
    fn specification(&self) -> CommandSpec {
        CommandSpec {
            name: "review".into(),
            description: "Review input".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn execute(
        &self,
        request: CommandRequest<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<CommandOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let ui = cancellation.ui();
        ui.notify(UiLevel::Info, "Ready to review".into())?;
        ui.status("review", Some("Waiting for data".into()))?;
        ui.ask(UiPrompt::Select {
            title: "Review action".into(),
            options: vec![UiOption {
                id: "continue".into(),
                label: "Continue".into(),
            }],
        })
        .await?;
        let UiAnswer::Text { text } = ui
            .ask(UiPrompt::Input {
                title: "Review note".into(),
                initial: String::new(),
                multiline: true,
                max_bytes: 128,
            })
            .await?
        else {
            return Ok(CommandOutput::Notice("Dismissed".into()));
        };
        if request.arguments["submit"] == true {
            Ok(CommandOutput::Submit(
                format!("Review request: {text}").into(),
            ))
        } else {
            Ok(CommandOutput::Notice(format!("Review noted: {text}")))
        }
    }
}
struct Guard;
#[async_trait]
impl ToolHook for Guard {
    async fn before_tool(
        &self,
        _: ToolHookInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<ToolHookDecision> {
        cancellation
            .ui()
            .ask(UiPrompt::Select {
                title: "Tool input choice".into(),
                options: vec![UiOption {
                    id: "continue".into(),
                    label: "Continue with this data".into(),
                }],
            })
            .await?;
        Ok(ToolHookDecision::Continue)
    }
}
struct Server {
    host: WebHost,
    url: String,
    client: reqwest::Client,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    root: tempfile::TempDir,
    model: Arc<Model>,
    commands: Arc<AtomicUsize>,
}
impl Server {
    async fn new(write: bool) -> Self {
        Self::with_input(write, false).await
    }
    async fn with_input(write: bool, inputs: bool) -> Self {
        Self::with_setup(write, inputs, false).await
    }
    async fn with_setup(write: bool, inputs: bool, messages: bool) -> Self {
        Self::with_session_hook(write, inputs, messages, None).await
    }
    async fn with_session_hook(
        write: bool,
        inputs: bool,
        messages: bool,
        session: Option<Arc<dyn SessionHook>>,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        if inputs {
            std::fs::create_dir_all(root.path().join(".aicoder/prompts")).unwrap();
            std::fs::write(
                root.path().join(".aicoder/prompts/task.md"),
                "WEB_TEMPLATE $1",
            )
            .unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let runtime = Runtime::load_with_environment(
            root.path(),
            RuntimeOverrides {
                settings: Settings {
                    resources: ResourceSettings {
                        enabled: Setting::Value(inputs),
                        global: Setting::Value(false),
                        parents: Setting::Value(false),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                session_dir: Setting::Clear,
                ..Default::default()
            },
            ConfigDiscovery {
                enabled: false,
                ..Default::default()
            },
            Arc::new(BTreeMap::<String, String>::new()),
        )
        .unwrap();
        let commands = Arc::new(AtomicUsize::new(0));
        let mut extension = Extension::new("ui-test", "1").unwrap();
        extension
            .command(Arc::new(Review(commands.clone())), Duration::from_secs(30))
            .unwrap();
        let mut spec = HookSpec::new("ask-before-tool");
        spec.timeout = Duration::from_secs(10);
        extension.tool_hook(spec, Arc::new(Guard)).unwrap();
        if inputs {
            extension
                .command(Arc::new(WebCustom), Duration::from_secs(2))
                .unwrap();
        }
        if inputs {
            extension
                .input_hook(HookSpec::new("raw"), Arc::new(WebInput))
                .unwrap();
        }
        if messages {
            extension
                .message_hook(HookSpec::new("finalize"), Arc::new(WebFinalize))
                .unwrap();
        }
        if let Some(hook) = session {
            extension
                .session_hook(HookSpec::new("session"), hook)
                .unwrap();
        }
        let mut registry = ExtensionRegistry::new();
        registry.install(extension).unwrap();
        let model = Arc::new(Model {
            write,
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        });
        let host = WebHost::with_provider(
            runtime,
            WebOptions::new(TOKEN.into(), address)
                .unwrap()
                .memory_sessions()
                .extensions(Arc::new(registry.finish())),
            model.clone(),
        )
        .unwrap();
        let router = host.router();
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        Self {
            host,
            url: format!("http://{address}"),
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            server,
            root,
            model,
            commands,
        }
    }
    async fn get(&self, path: &str) -> Value {
        self.client
            .get(format!("{}{path}", self.url))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn post(&self, path: &str, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}{path}", self.url))
            .bearer_auth(TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap()
    }
    async fn wait(&self, path: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let value = self.get(path).await;
                if predicate(&value) {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn command(&self, id: u64, submit: bool) -> String {
        let response = self
            .post(
                "/api/commands",
                json!({"id":id,"name":"review","arguments":{"submit":submit}}),
            )
            .await;
        assert_eq!(response.status(), 202);
        response.json::<Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
    async fn answer(&self, path: &str, text: &str) {
        let value = self
            .wait(path, |v| {
                v["ui"]["questions"]
                    .as_array()
                    .is_some_and(|q| !q.is_empty())
            })
            .await;
        let q = &value["ui"]["questions"][0];
        let answer = if q["prompt"]["kind"] == "select" {
            json!({"kind":"choice","id":"continue"})
        } else {
            json!({"kind":"text","text":text})
        };
        assert_eq!(
            self.post(
                &format!("{path}/ui"),
                json!({"question_id":q["id"],"answer":answer})
            )
            .await
            .status(),
            200
        );
    }
    async fn close(self) {
        self.host.shutdown().await;
        self.server.abort();
        let _ = self.server.await;
    }
}
#[tokio::test]
async fn authenticated_commands_ask_typed_questions_and_submit_only_explicit_agent_input() {
    let server = Server::new(false).await;
    assert_eq!(
        server
            .client
            .get(format!("{}/api/commands", server.url))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        server.get("/api/commands").await["commands"][0]["name"],
        "review"
    );
    let id = server.command(1, false).await;
    let path = format!("/api/commands/{id}");
    let waiting = server
        .wait(&path, |v| {
            v["ui"]["questions"]
                .as_array()
                .is_some_and(|q| !q.is_empty())
        })
        .await;
    assert_eq!(waiting["ui"]["notices"][0]["text"], "Ready to review");
    assert_eq!(waiting["ui"]["statuses"][0]["text"], "Waiting for data");
    assert_eq!(server.post(&format!("{path}/ui"),json!({"question_id":waiting["ui"]["questions"][0]["id"],"answer":{"kind":"choice","id":"continue","decision":"allow_session"}})).await.status(),422);
    server.answer(&path, "unused").await;
    server.answer(&path, "operator note").await;
    let done = server.wait(&path, |v| !v["result"].is_null()).await;
    assert_eq!(done["result"]["notice"], "Review noted: operator note");
    assert_eq!(done["ui"]["closed"], true);
    assert!(server.get("/api/state").await["run"].is_null());
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        server
            .post(
                "/api/commands",
                json!({"id":1,"name":"review","arguments":{}})
            )
            .await
            .status(),
        409
    );
    assert_eq!(server.commands.load(Ordering::SeqCst), 1);
    let id = server.command(2, true).await;
    let path = format!("/api/commands/{id}");
    server.answer(&path, "").await;
    server.answer(&path, "to the agent").await;
    let done = server.wait(&path, |v| !v["result"].is_null()).await;
    assert_eq!(done["result"]["status"], "submitted");
    let run = done["result"]["run_id"].as_str().unwrap();
    server
        .wait(&format!("/api/runs/{run}"), |v| !v["result"].is_null())
        .await;
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 1);
    assert!(
        server.model.requests.lock().unwrap()[0]
            .messages
            .last()
            .unwrap()
            .text_content()
            .contains("Review request: to the agent")
    );
    server.close().await;
}
#[tokio::test]
async fn cancelled_and_foreign_command_questions_cannot_reopen_or_submit_runs() {
    let server = Server::new(false).await;
    let id = server.command(1, true).await;
    let path = format!("/api/commands/{id}");
    let value = server
        .wait(&path, |v| {
            v["ui"]["questions"]
                .as_array()
                .is_some_and(|q| !q.is_empty())
        })
        .await;
    let old = value["ui"]["questions"][0]["id"].clone();
    assert_eq!(
        server
            .post(&format!("{path}/cancel"), json!({}))
            .await
            .status(),
        200
    );
    server.wait(&path, |v| !v["result"].is_null()).await;
    assert_eq!(
        server
            .post(
                &format!("{path}/ui"),
                json!({"question_id":old,"answer":{"kind":"choice","id":"continue"}})
            )
            .await
            .status(),
        409
    );
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 0);
    let id = server.command(2, false).await;
    let path = format!("/api/commands/{id}");
    server
        .wait(&path, |v| {
            v["ui"]["questions"]
                .as_array()
                .is_some_and(|q| !q.is_empty())
        })
        .await;
    assert_eq!(
        server
            .post(
                &format!("{path}/ui"),
                json!({"question_id":old,"answer":{"kind":"choice","id":"continue"}})
            )
            .await
            .status(),
        409
    );
    assert_eq!(
        server.get(&path).await["ui"]["questions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    server.host.shutdown().await;
    let done = server.get(&path).await;
    assert_eq!(done["ui"]["closed"], true);
    assert!(done["ui"]["questions"].as_array().unwrap().is_empty());
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 0);
    server.close().await;
}
#[tokio::test]
async fn ui_choices_never_resolve_tool_approvals_or_restore_session_grants() {
    let server = Server::new(true).await;
    let response = server
        .post("/api/runs", json!({"id":1,"text":"write"}))
        .await;
    assert_eq!(response.status(), 202);
    let id = response.json::<Value>().await.unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/runs/{id}");
    let pending = server
        .wait(&path, |v| {
            v["ui"]["questions"]
                .as_array()
                .is_some_and(|q| !q.is_empty())
        })
        .await;
    assert!(pending["approvals"].as_array().unwrap().is_empty());
    server.answer(&path, "").await;
    let pending = server
        .wait(&path, |v| {
            v["approvals"].as_array().is_some_and(|q| !q.is_empty())
        })
        .await;
    assert!(!server.root.path().join("result.txt").exists());
    assert_eq!(server.post(&format!("{path}/ui"),json!({"question_id":pending["approvals"][0]["id"],"answer":{"kind":"choice","id":"allow_session"}})).await.status(),422);
    assert_eq!(
        server
            .post(
                &format!("{path}/approval"),
                json!({"approval_id":pending["approvals"][0]["id"],"decision":"deny"})
            )
            .await
            .status(),
        200
    );
    server.wait(&path, |v| !v["result"].is_null()).await;
    assert!(!server.root.path().join("result.txt").exists());
    server.close().await;
}

struct WebInput;
#[async_trait]
impl InputHook for WebInput {
    async fn input(
        &self,
        input: InputHookInput<'_>,
        _: ExtensionCancellation,
    ) -> anyhow::Result<InputAction> {
        assert_eq!(input.source, InputSource::Web);
        Ok(if input.input.message().text_content() == "consume-web" {
            InputAction::Handled
        } else {
            InputAction::Transform(input.input.clone().with_prompt_text("/task transformed"))
        })
    }
}
#[tokio::test]
async fn authenticated_web_runs_transform_before_templates_and_report_consumed_input_without_model_io()
 {
    let server = Server::with_input(false, true).await;
    let response = server
        .post("/api/runs", json!({"id":1,"text":"raw-web"}))
        .await;
    assert_eq!(response.status(), 202);
    let run = response.json::<Value>().await.unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = server
        .wait(&format!("/api/runs/{run}"), |v| !v["result"].is_null())
        .await;
    assert_eq!(done["result"]["status"], "completed");
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 1);
    assert!(
        serde_json::to_string(&server.model.requests.lock().unwrap()[0].messages)
            .unwrap()
            .contains("WEB_TEMPLATE transformed")
    );
    let response = server
        .post("/api/runs", json!({"id":2,"text":"consume-web"}))
        .await;
    assert_eq!(response.status(), 202);
    let run = response.json::<Value>().await.unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = server
        .wait(&format!("/api/runs/{run}"), |v| !v["result"].is_null())
        .await;
    assert_eq!(done["result"]["status"], "handled");
    assert_eq!(done["result"]["stop"]["kind"], "input_handled");
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 1);
    server.close().await;
}

struct WebCustom;
#[async_trait]
impl ExtensionCommand for WebCustom {
    fn specification(&self) -> CommandSpec {
        CommandSpec {
            name: "custom".into(),
            description: "custom trigger".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn execute(
        &self,
        _: CommandRequest<'_>,
        _: ExtensionCancellation,
    ) -> anyhow::Result<CommandOutput> {
        Ok(CommandOutput::SubmitCustom(
            CustomMessage::text("unbound", "wake", "CUSTOM_WEB_FACT")?
                .with_details(json!({"private":"CUSTOM_WEB_DETAILS"}))?,
        ))
    }
}
#[tokio::test]
async fn authenticated_custom_commands_trigger_the_shared_agent_with_bound_source_and_inert_details()
 {
    let server = Server::with_input(false, true).await;
    let response = server
        .post(
            "/api/commands",
            json!({"id":1,"name":"custom","arguments":{}}),
        )
        .await;
    assert_eq!(response.status(), 202);
    let id = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = server
        .wait(&format!("/api/commands/{id}"), |v| !v["result"].is_null())
        .await;
    assert_eq!(done["result"]["status"], "submitted");
    let run = done["result"]["run_id"].as_str().unwrap();
    let done = server
        .wait(&format!("/api/runs/{run}"), |v| !v["result"].is_null())
        .await;
    assert_eq!(done["result"]["status"], "completed");
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 1);
    let body = {
        let requests = server.model.requests.lock().unwrap();
        serde_json::to_string(&requests[0].messages).unwrap()
    };
    assert!(body.contains("CUSTOM_WEB_FACT") && body.contains("aicoder custom ui-test/wake"));
    assert!(!body.contains("CUSTOM_WEB_DETAILS"));
    server.close().await;
}

struct WebFinalize;
#[async_trait]
impl MessageHook for WebFinalize {
    async fn finalize(
        &self,
        input: MessageHookInput<'_>,
        _: ExtensionCancellation,
    ) -> anyhow::Result<Option<Message>> {
        Ok(match input.message.role {
            MessageRole::User => Some(Message::text(MessageRole::User, "WEB_FINAL_USER")),
            MessageRole::Assistant => {
                Some(Message::text(MessageRole::Assistant, "WEB_FINAL_ASSISTANT"))
            }
            _ => None,
        })
    }
}
#[tokio::test]
async fn authenticated_web_sse_delivers_effective_user_and_assistant_after_finalization() {
    let server = Server::with_setup(false, false, true).await;
    assert_eq!(
        server
            .client
            .post(format!("{}/api/runs", server.url))
            .json(&json!({"id":1,"text":"private input"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 0);
    let response = server
        .post("/api/runs", json!({"id":1,"text":"private input"}))
        .await;
    assert_eq!(response.status(), 202);
    let run = response.json::<Value>().await.unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = server
        .wait(&format!("/api/runs/{run}"), |v| !v["result"].is_null())
        .await;
    assert_eq!(done["result"]["status"], "completed");
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        server.model.requests.lock().unwrap()[0]
            .messages
            .last()
            .unwrap()
            .text_content(),
        "WEB_FINAL_USER"
    );
    let text = server
        .client
        .get(format!("{}/api/runs/{run}/events", server.url))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    let frames = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let finalized = frames
        .iter()
        .filter(|f| f["event"]["type"] == "message_finalized")
        .collect::<Vec<_>>();
    assert_eq!(finalized.len(), 2);
    assert_eq!(
        finalized[0]["event"]["message"]["content"][0]["text"],
        "WEB_FINAL_USER"
    );
    assert_eq!(
        finalized[1]["event"]["message"]["content"][0]["text"],
        "WEB_FINAL_ASSISTANT"
    );
    assert!(!text.contains("private input"));
    server.close().await;
}

struct DenySession;
#[async_trait]
impl SessionHook for DenySession {
    async fn before(
        &self,
        _: SessionHookInput<'_>,
        _: ExtensionCancellation,
    ) -> anyhow::Result<SessionDecision> {
        Ok(SessionDecision::Deny {
            reason: "host session policy".into(),
        })
    }
}
#[tokio::test]
async fn authenticated_web_session_veto_prevents_model_io_and_reports_failed_preparation() {
    let server = Server::with_session_hook(false, false, false, Some(Arc::new(DenySession))).await;
    let response = server
        .post("/api/runs", json!({"id":1,"text":"user"}))
        .await;
    assert_eq!(response.status(), 202);
    let run = response.json::<Value>().await.unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = server
        .wait(&format!("/api/runs/{run}"), |v| !v["result"].is_null())
        .await;
    assert_eq!(done["result"]["status"], "failed");
    assert!(done["result"]["session_id"].is_null());
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 0);
    server.close().await;
}
struct LifecycleEvents(Mutex<Vec<SessionLifecycleEvent>>);
#[async_trait]
impl SessionHook for LifecycleEvents {
    async fn after(
        &self,
        event: &SessionLifecycleEvent,
        _: ExtensionCancellation,
    ) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}
#[tokio::test]
async fn web_session_start_is_once_per_binding_and_shutdown_waits_owner_and_notifies_once() {
    let hooks = Arc::new(LifecycleEvents(Mutex::new(Vec::new())));
    let server = Server::with_session_hook(false, false, false, Some(hooks.clone())).await;
    for id in 1..=2 {
        let response = server
            .post("/api/runs", json!({"id":id,"text":"user"}))
            .await;
        let run = response.json::<Value>().await.unwrap()["run_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let done = server
            .wait(&format!("/api/runs/{run}"), |v| !v["result"].is_null())
            .await;
        assert_eq!(done["result"]["status"], "completed");
    }
    assert_eq!(
        hooks
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e.kind, SessionEventKind::Started))
            .count(),
        1
    );
    server.host.shutdown().await;
    server.host.shutdown().await;
    assert_eq!(
        hooks
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e.kind, SessionEventKind::Shutdown))
            .count(),
        1
    );
    server.close().await;
}

struct ForkChoice;
#[async_trait]
impl SessionHook for ForkChoice {
    async fn before(
        &self,
        input: SessionHookInput<'_>,
        c: ExtensionCancellation,
    ) -> anyhow::Result<SessionDecision> {
        if !matches!(input.operation, SessionOperation::Fork { .. }) {
            return Ok(SessionDecision::Continue);
        }
        let answer = c
            .ui()
            .ask(UiPrompt::Select {
                title: "Fork session?".into(),
                options: vec![
                    UiOption {
                        id: "allow".into(),
                        label: "Allow".into(),
                    },
                    UiOption {
                        id: "deny".into(),
                        label: "Deny".into(),
                    },
                ],
            })
            .await?;
        Ok(if matches!(answer,UiAnswer::Choice{id}if id=="allow") {
            SessionDecision::Continue
        } else {
            SessionDecision::Deny {
                reason: "host denied fork".into(),
            }
        })
    }
}
#[tokio::test]
async fn web_fork_authenticates_reserves_one_owner_and_adopts_only_a_completed_new_session() {
    let server = Server::with_session_hook(false, false, false, Some(Arc::new(ForkChoice))).await;
    let response = server
        .post("/api/runs", json!({"id":1,"text":"original"}))
        .await;
    let run = response.json::<Value>().await.unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = server
        .wait(&format!("/api/runs/{run}"), |v| !v["result"].is_null())
        .await;
    let source = done["result"]["session_id"].as_str().unwrap().to_owned();
    assert_eq!(
        server
            .client
            .post(format!("{}/api/session/fork", server.url))
            .json(&json!({"id":2,"point":{"position":"current"}}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = server
        .post(
            "/api/session/fork",
            json!({"id":2,"point":{"position":"current"}}),
        )
        .await;
    assert_eq!(response.status(), 202);
    let op = response.json::<Value>().await.unwrap()["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = server
        .wait("/api/session/operation", |v| {
            v["operation"]["ui"]["questions"]
                .as_array()
                .is_some_and(|q| !q.is_empty())
        })
        .await;
    assert_eq!(
        server
            .post("/api/runs", json!({"id":3,"text":"busy"}))
            .await
            .status(),
        409
    );
    assert_eq!(
        server
            .post("/api/session/operation/foreign/cancel", json!({}))
            .await
            .status(),
        404
    );
    let question = waiting["operation"]["ui"]["questions"][0]["id"].clone();
    assert_eq!(server.post(&format!("/api/session/operation/{op}/ui"),json!({"question_id":question,"answer":{"kind":"choice","id":"allow","decision":"allow_session"}})).await.status(),422);
    assert_eq!(
        server
            .post(
                &format!("/api/session/operation/{op}/ui"),
                json!({"question_id":question,"answer":{"kind":"choice","id":"allow"}})
            )
            .await
            .status(),
        200
    );
    let finished = server
        .wait("/api/session/operation", |v| {
            !v["operation"]["result"].is_null()
        })
        .await;
    assert_eq!(finished["operation"]["result"]["status"], "completed");
    let target = finished["operation"]["result"]["session"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(target, source);
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        server
            .post(
                "/api/session/fork",
                json!({"id":2,"point":{"position":"current"}})
            )
            .await
            .status(),
        409
    );
    let response = server
        .post("/api/runs", json!({"id":4,"text":"continued"}))
        .await;
    let run = response.json::<Value>().await.unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = server
        .wait(&format!("/api/runs/{run}"), |v| !v["result"].is_null())
        .await;
    assert_eq!(done["result"]["session_id"], target);
    assert!(
        server.model.requests.lock().unwrap()[1]
            .messages
            .iter()
            .any(|m| m.text_content() == "original")
    );
    server.close().await;
}
#[tokio::test]
async fn web_fork_cancellation_and_shutdown_expire_private_choices_without_creating_late_targets() {
    let server = Server::with_session_hook(false, false, false, Some(Arc::new(ForkChoice))).await;
    let response = server
        .post("/api/runs", json!({"id":1,"text":"original"}))
        .await;
    let run = response.json::<Value>().await.unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = server
        .wait(&format!("/api/runs/{run}"), |v| !v["result"].is_null())
        .await;
    let source = done["result"]["session_id"].clone();
    let response = server
        .post(
            "/api/session/fork",
            json!({"id":2,"point":{"position":"before_last_user"}}),
        )
        .await;
    let op = response.json::<Value>().await.unwrap()["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = server
        .wait("/api/session/operation", |v| {
            v["operation"]["ui"]["questions"]
                .as_array()
                .is_some_and(|q| !q.is_empty())
        })
        .await;
    let question = waiting["operation"]["ui"]["questions"][0]["id"].clone();
    assert_eq!(
        server
            .post(&format!("/api/session/operation/{op}/cancel"), json!({}))
            .await
            .status(),
        200
    );
    let done = server
        .wait("/api/session/operation", |v| {
            !v["operation"]["result"].is_null()
        })
        .await;
    assert_eq!(done["operation"]["result"]["status"], "failed");
    assert_eq!(
        server
            .post(
                &format!("/api/session/operation/{op}/ui"),
                json!({"question_id":question,"answer":{"kind":"choice","id":"allow"}})
            )
            .await
            .status(),
        409
    );
    let response = server
        .post(
            "/api/session/fork",
            json!({"id":3,"point":{"position":"current"}}),
        )
        .await;
    assert_eq!(response.status(), 202);
    server
        .wait("/api/session/operation", |v| {
            v["operation"]["ui"]["questions"]
                .as_array()
                .is_some_and(|q| !q.is_empty())
        })
        .await;
    server.host.shutdown().await;
    let done = server.get("/api/session/operation").await;
    assert_eq!(done["operation"]["result"]["status"], "failed");
    assert_eq!(
        server.get(&format!("/api/runs/{run}")).await["result"]["session_id"],
        source
    );
    assert_eq!(server.model.calls.load(Ordering::SeqCst), 1);
    server.close().await;
}
