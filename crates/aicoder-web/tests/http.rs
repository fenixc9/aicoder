#![cfg(feature = "server")]
use aicoder_core::{Agent, TurnExecutionContext, model::*, session::SessionRepository};
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
const TOKEN: &str = "fixture-web-token-0123456789abcdef";
#[derive(Clone, Copy)]
enum Mode {
    Answer,
    Write,
    Wait,
    Flood,
    Panic,
}
struct Script {
    mode: Mode,
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
}
impl Script {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            calls: AtomicUsize::new(0),
            requests: Mutex::new(vec![]),
        })
    }
}
#[async_trait]
impl ModelProvider for Script {
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
        if matches!(self.mode, Mode::Wait) {
            std::future::pending::<()>().await;
        }
        if matches!(self.mode, Mode::Panic) {
            panic!("offline owner failure")
        }
        if matches!(self.mode, Mode::Flood) {
            for _ in 0..1100 {
                sink.emit(ModelEvent::TextDelta {
                    delta: "part".into(),
                });
            }
        }
        let tool = matches!(self.mode, Mode::Write)
            && request
                .messages
                .last()
                .is_none_or(|m| m.role != MessageRole::Tool);
        let message = if tool {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "write".into(),
                        name: "write_file".into(),
                        arguments: json!({"path":"result.txt","content":"approved"}),
                    },
                }],
            }
        } else {
            Message::text(MessageRole::Assistant, "same answer")
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if tool {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage {
                input_tokens: Some(1),
                output_tokens: Some(1),
                total_tokens: Some(2),
                ..Default::default()
            },
        };
        if !matches!(self.mode, Mode::Flood) {
            emit_response(&response, sink.as_ref());
        }
        Ok(response)
    }
}
fn runtime(workspace: &std::path::Path, storage: Option<&std::path::Path>) -> Runtime {
    Runtime::load_with_environment(
        workspace,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: storage.map_or(Setting::Clear, |p| Setting::Value(p.into())),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::new()),
    )
    .unwrap()
}
struct Server {
    host: WebHost,
    client: reqwest::Client,
    url: String,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    root: tempfile::TempDir,
}
impl Server {
    async fn new(mode: Mode, persistent: bool) -> (Self, Arc<Script>) {
        let root = tempfile::tempdir().unwrap();
        let provider = Script::new(mode);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut options = WebOptions::new(TOKEN.into(), address).unwrap();
        if !persistent {
            options = options.memory_sessions();
        }
        let storage = root.path().join("state");
        let application = runtime(root.path(), persistent.then_some(storage.as_path()));
        let host = WebHost::with_provider(application, options, provider.clone()).unwrap();
        let router = host.router();
        let task = tokio::spawn(async move { axum::serve(listener, router).await });
        (
            Self {
                host,
                client: reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(8))
                    .build()
                    .unwrap(),
                url: format!("http://{address}"),
                task,
                root,
            },
            provider,
        )
    }
    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}{path}", self.url))
            .bearer_auth(TOKEN)
    }
    fn post(&self, path: &str, body: Value) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", self.url))
            .bearer_auth(TOKEN)
            .json(&body)
    }
    async fn start(&self, id: u64) -> String {
        let response = self
            .post("/api/runs", json!({"id":id,"text":"parity"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202);
        response.json::<Value>().await.unwrap()["run_id"]
            .as_str()
            .unwrap()
            .into()
    }
    async fn status(&self, id: &str) -> Value {
        self.get(&format!("/api/runs/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn wait(&self, id: &str, approval: bool) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = self.status(id).await;
                if approval {
                    if !status["approvals"].as_array().unwrap().is_empty() {
                        return status;
                    }
                } else if !status["result"].is_null() {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("Web owner/status deadline")
    }
    async fn trace(&self, id: &str) -> Vec<(String, Value)> {
        let response = self
            .get(&format!("/api/runs/{id}/events"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        let text = response.text().await.unwrap();
        parse_trace(&text)
    }
    async fn close(self) {
        self.host.shutdown().await;
        self.task.abort();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn parse_trace(text: &str) -> Vec<(String, Value)> {
    let mut frames = vec![];
    for block in text.split("\n\n") {
        let kind = block
            .lines()
            .find_map(|l| l.strip_prefix("event:").map(str::trim));
        let data = block
            .lines()
            .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(kind) = kind {
            frames.push((kind.into(), serde_json::from_str(&data).unwrap()));
        }
    }
    frames
}
fn validate(frames: &[(String, Value)]) {
    let mut validator = aicoder_core::events::EventTraceValidator::new();
    for (kind, frame) in frames {
        if kind == "agent" {
            validator
                .observe(&serde_json::from_value(frame.clone()).unwrap())
                .unwrap();
        }
    }
    validator.finish().unwrap();
    assert_eq!(frames.last().unwrap().0, "result");
}
#[tokio::test]
async fn authentication_origin_body_and_workspace_are_host_owned() {
    let (server, provider) = Server::new(Mode::Answer, false).await;
    assert_eq!(
        server
            .client
            .get(format!("{}/api/state", server.url))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        server
            .client
            .get(format!("{}/api/state?token={TOKEN}", server.url))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        server
            .get("/api/state")
            .header("Origin", "https://external.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        server
            .get("/api/state")
            .header("Host", "external.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        server
            .client
            .get(format!("{}/api/state", server.url))
            .bearer_auth("wrong-token-01234567890123456789012")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        server
            .post("/api/runs", json!({"id":1,"text":"x","workspace":"/"}))
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    assert_eq!(
        server
            .post("/api/runs", json!({"id":1,"text":"x".repeat(1024*1024)}))
            .send()
            .await
            .unwrap()
            .status(),
        413
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let page = server.client.get(&server.url).send().await.unwrap();
    assert!(
        page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );
    assert_eq!(page.headers()["cache-control"], "no-store");
    assert!(!page.text().await.unwrap().contains(TOKEN));
    let js = server
        .client
        .get(format!("{}/app.js", server.url))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(js.contains("textContent"));
    assert!(!js.contains("innerHTML"));
    assert!(!js.contains("localStorage"));
    server.close().await;
}
#[tokio::test]
async fn web_sse_and_sdk_have_equal_normalized_results_and_memory_history() {
    let (server, provider) = Server::new(Mode::Answer, false).await;
    let sdk_runtime = runtime(server.root.path(), None);
    let sdk = Agent::new(
        sdk_runtime
            .executor_builder(provider.clone())
            .unwrap()
            .build()
            .unwrap(),
        sdk_runtime.agent_config(),
    )
    .run_outcome("parity", Arc::new(()), TurnExecutionContext::new())
    .await
    .into_result()
    .unwrap();
    let run = server.start(1).await;
    let frames = server.trace(&run).await;
    validate(&frames);
    let result = &frames.last().unwrap().1;
    let text = frames
        .iter()
        .filter(|(kind, event)| kind == "agent" && event["event"]["type"] == "content_chunk")
        .map(|(_, event)| event["event"]["delta"].as_str().unwrap())
        .collect::<String>();
    assert_eq!(
        json!({"text":text,"rounds":result["rounds"],"usage":result["usage"]}),
        json!({"text":sdk.execution_result.final_message.text_content(),"rounds":sdk.execution_result.progress.rounds,"usage":sdk.execution_result.progress.usage})
    );
    assert_eq!(
        server
            .post("/api/runs", json!({"id":1,"text":"must not repeat"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        server
            .get(&format!("/api/runs/{run}/events"))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let next = server.start(2).await;
    let frames = server.trace(&next).await;
    validate(&frames);
    assert_eq!(frames.last().unwrap().1["session_id"], result["session_id"]);
    assert_eq!(
        server
            .post(&format!("/api/runs/{run}/cancel"), json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert!(
        provider
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .count()
            >= 2
    );
    server.close().await;
}
#[tokio::test]
async fn exact_approval_choices_commit_or_deny_the_prepared_write() {
    for allow in [false, true] {
        let (server, _) = Server::new(Mode::Write, true).await;
        let run = server.start(1).await;
        let status = server.wait(&run, true).await;
        let approval = &status["approvals"][0];
        assert!(
            approval["invocation"]["preview"]
                .to_string()
                .contains("result.txt")
        );
        let mut wrong = approval["id"].clone();
        wrong["run_id"] = json!("other-run");
        assert_eq!(
            server
                .post(
                    &format!("/api/runs/{run}/approval"),
                    json!({"approval_id":wrong,"decision":"allow_once"})
                )
                .send()
                .await
                .unwrap()
                .status(),
            409
        );
        let choice = if allow { "allow_session" } else { "deny" };
        assert_eq!(
            server
                .post(
                    &format!("/api/runs/{run}/approval"),
                    json!({"approval_id":approval["id"],"decision":choice})
                )
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let frames = server.trace(&run).await;
        validate(&frames);
        assert_eq!(frames.last().unwrap().1["status"], "completed");
        assert_eq!(server.root.path().join("result.txt").exists(), allow);
        assert_eq!(
            server
                .post(
                    &format!("/api/runs/{run}/approval"),
                    json!({"approval_id":approval["id"],"decision":"allow_once"})
                )
                .send()
                .await
                .unwrap()
                .status(),
            409
        );
        server.close().await;
    }
}
#[tokio::test]
async fn disconnect_does_not_cancel_but_http_cancel_awaits_persisted_completion() {
    let (server, provider) = Server::new(Mode::Wait, true).await;
    let run = server.start(1).await;
    let response = server
        .get(&format!("/api/runs/{run}/events"))
        .send()
        .await
        .unwrap();
    drop(response);
    tokio::time::timeout(Duration::from_secs(3), async {
        while provider.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(server.status(&run).await["result"].is_null());
    assert_eq!(
        server
            .post("/api/runs", json!({"id":2,"text":"busy"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        server
            .post(&format!("/api/runs/{run}/cancel"), json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let status = server.wait(&run, false).await;
    assert_eq!(status["result"]["status"], "aborted");
    let repo =
        aicoder_runtime::session::JsonlSessionRepository::new(server.root.path().join("state"))
            .unwrap();
    assert!(
        repo.open(status["result"]["session_id"].as_str().unwrap())
            .unwrap()
            .unfinished_turn()
            .is_none()
    );
    server.close().await;
}
#[tokio::test]
async fn shutdown_closes_admission_and_expires_pending_prepared_approval() {
    let (server, _) = Server::new(Mode::Write, true).await;
    let run = server.start(1).await;
    let pending = server.wait(&run, true).await;
    let id = pending["approvals"][0]["id"].clone();
    server.host.shutdown().await;
    let status = server.status(&run).await;
    assert_eq!(status["result"]["status"], "aborted");
    assert_eq!(
        server
            .post("/api/runs", json!({"id":2,"text":"late"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        server
            .post(
                &format!("/api/runs/{run}/approval"),
                json!({"approval_id":id,"decision":"allow_session"})
            )
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert!(!server.root.path().join("result.txt").exists());
    let repo =
        aicoder_runtime::session::JsonlSessionRepository::new(server.root.path().join("state"))
            .unwrap();
    assert!(
        repo.open(status["result"]["session_id"].as_str().unwrap())
            .unwrap()
            .unfinished_turn()
            .is_none()
    );
    server.close().await;
}
#[tokio::test]
async fn overflow_is_explicit_and_an_owner_panic_cannot_leave_admission_busy() {
    let (server, _) = Server::new(Mode::Flood, false).await;
    let run = server.start(1).await;
    server.wait(&run, false).await;
    let frames = server.trace(&run).await;
    assert!(frames.iter().any(|(name, _)| name == "observation_error"));
    assert_eq!(frames.last().unwrap().0, "result");
    assert_eq!(frames.last().unwrap().1["status"], "completed");
    server.close().await;
    let (server, provider) = Server::new(Mode::Panic, false).await;
    let run = server.start(1).await;
    let first = server.wait(&run, false).await;
    assert_eq!(first["result"]["status"], "owner_failed");
    assert!(first["result"]["session_id"].is_string());
    let next = server.start(2).await;
    let second = server.wait(&next, false).await;
    assert_eq!(second["result"]["status"], "owner_failed");
    assert_eq!(
        second["result"]["session_id"],
        first["result"]["session_id"]
    );
    assert_eq!(
        provider
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .count(),
        2,
        "owner panic must retain the canonical user history"
    );
    server.close().await;
}

#[tokio::test]
async fn binary_sigterm_awaits_the_owned_turn_and_journal() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::mpsc,
    };
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = tempfile::tempdir().unwrap();
    let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_aicoder-web"))
            .env_clear()
            .env("AICODER_HOME", root.path().join("state"))
            .env("AICODER_WEB_TOKEN", TOKEN)
            .env("OPENAI_API_KEY", "fixture-key")
            .env("AICODER_PROXY", "disabled")
            .env("AICODER_MODEL", "test")
            .env(
                "AICODER_API_BASE",
                format!("http://{}/v1", provider.local_addr().unwrap()),
            )
            .args(["--no-config", "--no-resources", "--workspace"])
            .arg(root.path())
            .args(["--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = child.0.stderr.take().unwrap();
    let (sent, received) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stderr).read_line(&mut line).unwrap();
        let _ = sent.send(line);
    });
    let ready = received.recv_timeout(Duration::from_secs(5)).unwrap();
    let url = ready
        .trim()
        .strip_prefix("Web example listening at ")
        .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .post(format!("{url}/api/runs"))
        .bearer_auth(TOKEN)
        .json(&json!({"id":1,"text":"await provider"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let (socket, _) = tokio::time::timeout(Duration::from_secs(5), provider.accept())
        .await
        .unwrap()
        .unwrap();
    socket.readable().await.unwrap(); // A real model request reached the fixture before the signal.
    assert!(
        Command::new("bash")
            .args(["-c", &format!("kill -TERM {}", child.0.id())])
            .status()
            .unwrap()
            .success()
    );
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let repo =
        aicoder_runtime::session::JsonlSessionRepository::new(root.path().join("state/sessions"))
            .unwrap();
    let sessions = repo.list(root.path()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(
        repo.open(&sessions[0].id)
            .unwrap()
            .unfinished_turn()
            .is_none()
    );
}

#[tokio::test]
async fn revocation_expires_a_question_and_cannot_create_a_late_grant() {
    let (server, _) = Server::new(Mode::Write, true).await;
    let run = server.start(1).await;
    let pending = server.wait(&run, true).await;
    assert_eq!(
        server
            .post("/api/permissions/revoke", json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        server.wait(&run, false).await["result"]["status"],
        "completed"
    );
    assert_eq!(
        server
            .post(
                &format!("/api/runs/{run}/approval"),
                json!({"approval_id":pending["approvals"][0]["id"],"decision":"allow_session"})
            )
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert!(!server.root.path().join("result.txt").exists());
    server.close().await;
}
