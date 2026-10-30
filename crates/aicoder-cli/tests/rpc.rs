use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

fn command(root: &std::path::Path, endpoint: &str) -> Command {
    configured_command(root, endpoint, false)
}
fn configured_command(root: &std::path::Path, endpoint: &str, persistent: bool) -> Command {
    let mut c = base_command(root, endpoint, persistent);
    c.arg("--rpc");
    c
}
fn base_command(root: &std::path::Path, endpoint: &str, persistent: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aicoder"));
    c.env_clear()
        .env("AICODER_HOME", root.join("state"))
        .env("AICODER_MODEL", "test")
        .env("AICODER_API_BASE", endpoint)
        .env("OPENAI_API_KEY", "fixture-key")
        .env("AICODER_STREAM", "false")
        .env("AICODER_PROXY", "disabled")
        .env("AICODER_MAX_RETRIES", "0")
        .args(["--no-config", "--no-resources", "--workspace"])
        .arg(root);
    if !persistent {
        c.arg("--no-session");
    }
    c
}
struct Client {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<Value>,
    frames: Vec<Value>,
}
impl Client {
    fn new(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                let frame: Value =
                    serde_json::from_str(&line).expect("stdout is exclusively protocol JSONL");
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        Self {
            input: child.stdin.take(),
            child,
            output: rx,
            frames: vec![],
        }
    }
    fn raw(&mut self, text: &str) {
        self.input
            .as_mut()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        self.input.as_mut().unwrap().write_all(b"\n").unwrap();
    }
    fn send(&mut self, id: u64, command: Value) {
        self.raw(&json!({"version":1,"id":id,"command":command}).to_string())
    }
    fn read(&mut self) -> Value {
        let frame = self
            .output
            .recv_timeout(Duration::from_secs(8))
            .expect("RPC frame deadline");
        self.frames.push(frame.clone());
        frame
    }
    fn response(&mut self, id: u64) -> Value {
        loop {
            let v = self.read();
            if v["type"] == "response" && v["id"] == id {
                return v;
            }
        }
    }
    fn finish_run(&mut self) -> Value {
        loop {
            let v = self.read();
            if v["type"] == "run_finished" {
                return v["result"].clone();
            }
        }
    }
    fn wait(&mut self) -> std::process::ExitStatus {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(8),
                "RPC exit deadline"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn shutdown(&mut self, id: u64) {
        self.send(id, json!({"type":"shutdown"}));
        assert!(self.response(id)["ok"].as_bool().unwrap());
        assert!(self.wait().success());
    }
    fn validate_traces(&self) -> usize {
        let mut count = 0;
        let mut validator = aicoder_core::events::EventTraceValidator::new();
        for frame in &self.frames {
            if frame["type"] == "event" {
                let event: aicoder_core::AgentRawEventEnvelope =
                    serde_json::from_value(frame["event"].clone()).unwrap();
                validator.observe(&event).unwrap();
            }
            if frame["type"] == "run_finished" {
                validator.finish().unwrap();
                validator = aicoder_core::events::EventTraceValidator::new();
                count += 1;
            }
        }
        count
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn read_request(socket: &mut TcpStream) -> Value {
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let mut bytes = vec![];
    let mut buf = [0; 4096];
    loop {
        let n = socket.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let size = String::from_utf8_lossy(&bytes[..end])
                .lines()
                .find_map(|s| {
                    s.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + size {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap();
            }
        }
    }
}
fn reply(socket: &mut TcpStream, message: Value, finish: &str) {
    let body=json!({"id":"fixture","object":"chat.completion","created":1,"model":"test","choices":[{"index":0,"message":message,"finish_reason":finish}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}).to_string();
    write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
}
fn listener() -> (String, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    (
        format!("http://{}/v1", listener.local_addr().unwrap()),
        listener,
    )
}
fn accept(listener: &TcpListener) -> TcpStream {
    let start = Instant::now();
    loop {
        match listener.accept() {
            Ok((socket, _)) => return socket,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(start.elapsed() < Duration::from_secs(8));
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}
#[test]
fn rpc_validation_reserves_ids_and_conflicting_cli_modes_cannot_pollute_stdout() {
    let root = tempfile::tempdir().unwrap();
    let mut client = Client::new(command(root.path(), "http://127.0.0.1:1/v1"));
    client.raw("not JSON");
    assert_eq!(client.read()["error"]["code"], "invalid_request");
    client.raw(r#"{"version":99,"id":1,"command":{"type":"prompt","text":"never"}}"#);
    assert_eq!(client.response(1)["error"]["code"], "unsupported_version");
    client.send(1, json!({"type":"prompt","text":"never"}));
    assert_eq!(client.response(1)["error"]["code"], "stale_id");
    client.send(2, json!({"type":"initialize"}));
    let response = client.response(2);
    assert_eq!(response["result"]["protocol_version"], 1);
    assert_eq!(response["result"]["limits"]["active_runs"], 1);
    client.send(3, json!({"type":"prompt","text":""}));
    assert_eq!(client.response(3)["ok"], false);
    client.send(4, json!({"type":"status"}));
    assert!(client.response(4)["result"]["active"].is_null());
    client.shutdown(5);
    for flags in [
        vec!["--json"],
        vec!["--print"],
        vec!["--repl"],
        vec!["--prompt", "never"],
        vec!["config"],
    ] {
        let output = command(root.path(), "http://127.0.0.1:1/v1")
            .args(flags)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}
#[test]
fn rpc_turns_ack_before_events_keep_core_history_and_reject_reused_prompt_ids() {
    let root = tempfile::tempdir().unwrap();
    let (endpoint, listener) = listener();
    let server = std::thread::spawn(move || {
        let mut requests = vec![];
        for _ in 0..2 {
            let mut socket = accept(&listener);
            requests.push(read_request(&mut socket));
            reply(
                &mut socket,
                json!({"role":"assistant","content":"answer"}),
                "stop",
            );
        }
        requests
    });
    let mut client = Client::new(command(root.path(), &endpoint));
    let mut session = Value::Null;
    for id in 1..=2 {
        client.send(id, json!({"type":"prompt","text":format!("question{id}")}));
        let ack = client.read();
        assert_eq!(ack["type"], "response");
        assert_eq!(ack["id"], id);
        assert_eq!(ack["ok"], true);
        let result = client.finish_run();
        assert_eq!(result["run_id"], ack["result"]["run_id"]);
        assert_eq!(result["status"], "completed");
        assert_eq!(result["usage"]["total_tokens"], 2);
        if id == 1 {
            session = result["session_id"].clone();
        } else {
            assert_eq!(session, result["session_id"]);
        }
    }
    client.send(2, json!({"type":"prompt","text":"must not run"}));
    assert_eq!(client.response(2)["error"]["code"], "stale_id");
    client.send(
        3,
        json!({"type":"session_tree","session_id":session,"limit":3}),
    );
    let page = client.response(3);
    assert_eq!(page["ok"], true);
    assert!(page["result"]["rows"].as_array().unwrap().len() <= 3);
    client.shutdown(4);
    assert_eq!(client.validate_traces(), 2);
    assert!(!root.path().join("state/sessions").exists());
    let requests = server.join().unwrap();
    assert!(requests[1]["messages"].to_string().contains("question1"));
    assert!(requests[1]["messages"].to_string().contains("answer"));
}
#[test]
fn rpc_controls_bind_run_identity_and_cancel_retains_queued_input_metadata() {
    let root = tempfile::tempdir().unwrap();
    let (endpoint, listener) = listener();
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        read_request(&mut socket);
        tx.send(()).unwrap();
        let mut byte = [0];
        socket
            .set_read_timeout(Some(Duration::from_secs(7)))
            .unwrap();
        let _ = socket.read(&mut byte);
    });
    let mut client = Client::new(command(root.path(), &endpoint));
    client.send(1, json!({"type":"prompt","text":"waiting"}));
    let ack = client.response(1);
    let run = ack["result"]["run_id"].clone();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    client.send(2, json!({"type":"prompt","text":"busy"}));
    assert_eq!(client.response(2)["error"]["code"], "busy");
    client.send(3, json!({"type":"cancel","run_id":"old"}));
    assert_eq!(client.response(3)["error"]["code"], "unknown_run");
    for (id, kind) in [(4, "steer"), (5, "follow_up")] {
        client.send(id, json!({"type":kind,"run_id":run,"text":kind}));
        assert_eq!(client.response(id)["ok"], true);
    }
    client.send(6, json!({"type":"status"}));
    assert_eq!(
        client.response(6)["result"]["active"]["pending_inputs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    client.send(7, json!({"type":"cancel","run_id":run}));
    assert_eq!(client.response(7)["ok"], true);
    let result = client.finish_run();
    assert_eq!(result["status"], "aborted");
    assert_eq!(result["pending_inputs"].as_array().unwrap().len(), 2);
    client.send(8, json!({"type":"cancel","run_id":run}));
    assert_eq!(client.response(8)["error"]["code"], "unknown_run");
    client.shutdown(9);
    assert_eq!(client.validate_traces(), 1);
    server.join().unwrap();
}

fn approval(client: &mut Client) -> Value {
    loop {
        let frame = client.read();
        if frame["type"] == "event" && frame["event"]["event"]["type"] == "run_approval_requested" {
            return frame["event"]["event"]["request"].clone();
        }
    }
}
#[test]
fn rpc_session_choices_grants_and_revocation_remain_core_authority() {
    let root = tempfile::tempdir().unwrap();
    let (endpoint, listener) = listener();
    let server = std::thread::spawn(move || {
        let mut requests = vec![];
        for i in 0..6 {
            let mut socket = accept(&listener);
            requests.push(read_request(&mut socket));
            if i % 2 == 0 {
                reply(
                    &mut socket,
                    json!({"role":"assistant","content":null,"tool_calls":[{"id":format!("write{i}"),"type":"function","function":{"name":"write_file","arguments":json!({"path":"approved.txt","content":format!("version{i}")}).to_string()}}]}),
                    "tool_calls",
                );
            } else {
                reply(
                    &mut socket,
                    json!({"role":"assistant","content":"done"}),
                    "stop",
                );
            }
        }
        requests
    });
    let mut client = Client::new(command(root.path(), &endpoint));
    client.send(1, json!({"type":"prompt","text":"write once"}));
    let run = client.response(1)["result"]["run_id"].clone();
    let question = approval(&mut client);
    assert!(question["session"].is_object());
    assert!(
        question["invocation"]["preview"]
            .to_string()
            .contains("approved.txt")
    );
    client.send(2,json!({"type":"approve","run_id":"wrong","approval_id":question["id"],"decision":"allow_session"}));
    assert_eq!(client.response(2)["error"]["code"], "unknown_run");
    client.send(3,json!({"type":"approve","run_id":run,"approval_id":question["id"],"decision":"allow_session"}));
    assert_eq!(client.response(3)["ok"], true);
    assert_eq!(client.finish_run()["status"], "completed");
    assert_eq!(
        std::fs::read_to_string(root.path().join("approved.txt")).unwrap(),
        "version0"
    );
    client.send(4, json!({"type":"permissions"}));
    assert_eq!(
        client.response(4)["result"]["permissions"]["grants"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    client.send(
        5,
        json!({"type":"prompt","text":"same session cached authority"}),
    );
    client.response(5);
    assert_eq!(client.finish_run()["status"], "completed");
    assert_eq!(
        std::fs::read_to_string(root.path().join("approved.txt")).unwrap(),
        "version2"
    );
    client.send(6, json!({"type":"revoke"}));
    assert_eq!(client.response(6)["result"]["revoked"], true);
    client.send(7, json!({"type":"prompt","text":"new approval needed"}));
    let next_run = client.response(7)["result"]["run_id"].clone();
    let pending = approval(&mut client);
    client.send(8,json!({"type":"approve","run_id":next_run,"approval_id":question["id"],"decision":"allow_once"}));
    assert_eq!(client.response(8)["ok"], false);
    client.send(9, json!({"type":"revoke"}));
    assert_eq!(client.response(9)["ok"], true);
    // Revocation expires the in-flight question; no separate frontend permission rule.
    let result = client.finish_run();
    assert_eq!(result["status"], "completed");
    client.send(10,json!({"type":"approve","run_id":next_run,"approval_id":pending["id"],"decision":"allow_session"}));
    assert_eq!(client.response(10)["ok"], false);
    assert_eq!(
        std::fs::read_to_string(root.path().join("approved.txt")).unwrap(),
        "version2"
    );
    client.shutdown(11);
    assert_eq!(client.validate_traces(), 3);
    let requests = server.join().unwrap();
    assert!(requests[5]["messages"].to_string().contains("approval"));
}
#[test]
fn rpc_eof_and_sigint_cancel_and_await_the_same_owner() {
    for interrupt in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (endpoint, listener) = listener();
        let (tx, rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            read_request(&mut socket);
            tx.send(()).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(7)))
                .unwrap();
            let mut byte = [0];
            let _ = socket.read(&mut byte);
        });
        let mut client = Client::new(configured_command(root.path(), &endpoint, true));
        client.send(1, json!({"type":"prompt","text":"wait"}));
        client.response(1);
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if interrupt {
            unsafe {
                libc::kill(client.child.id() as i32, libc::SIGINT);
            }
        } else {
            drop(client.input.take());
        }
        let result = client.finish_run();
        assert_eq!(result["status"], "aborted");
        let status = client.wait();
        if interrupt {
            assert_eq!(status.code(), Some(130));
        } else {
            assert!(status.success());
        }
        assert_eq!(client.validate_traces(), 1);
        use aicoder_core::session::SessionRepository;
        let repo = aicoder_runtime::session::JsonlSessionRepository::new(
            root.path().join("state/sessions"),
        )
        .unwrap();
        let session = repo.open(result["session_id"].as_str().unwrap()).unwrap();
        assert!(
            session.unfinished_turn().is_none(),
            "journal must close before process exit"
        );
        server.join().unwrap();
    }
}
#[test]
fn rpc_nonreading_stdout_exits_with_explicit_transport_failure() {
    let root = tempfile::tempdir().unwrap();
    let (endpoint, listener) = listener();
    let server = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        read_request(&mut socket);
        reply(
            &mut socket,
            json!({"role":"assistant","content":"x".repeat(200000)}),
            "stop",
        );
    });
    let mut child = command(root.path(), &endpoint)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"{\"version\":1,\"id\":1,\"command\":{\"type\":\"prompt\",\"text\":\"large answer\"}}\n").unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(8) {
            let _ = child.kill();
            panic!("stalled transport kept owner alive")
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    let mut errors = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut errors)
        .unwrap();
    assert!(errors.contains("RPC stdout made no progress"), "{errors}");
    server.join().unwrap();
}

#[test]
fn rpc_session_reads_and_selection_reject_other_workspaces_and_stale_pages() {
    use aicoder_core::session::SessionRepository;
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let repo =
        aicoder_runtime::session::JsonlSessionRepository::new(root.path().join("state/sessions"))
            .unwrap();
    let foreign = repo.create(other.path()).unwrap();
    let local = repo.create(root.path()).unwrap();
    let mut client = Client::new(configured_command(
        root.path(),
        "http://127.0.0.1:1/v1",
        true,
    ));
    for (id, command) in [
        (
            1,
            json!({"type":"select_session","session_id":foreign.metadata().id}),
        ),
        (
            2,
            json!({"type":"session_tree","session_id":foreign.metadata().id,"limit":2}),
        ),
        (
            3,
            json!({"type":"session_search","session_id":foreign.metadata().id,"query":"x","limit":2}),
        ),
    ] {
        client.send(id, command);
        assert_eq!(client.response(id)["ok"], false);
    }
    client.send(
        4,
        json!({"type":"select_session","session_id":local.metadata().id}),
    );
    assert_eq!(
        client.response(4)["result"]["session_id"],
        local.metadata().id
    );
    client.send(5,json!({"type":"session_tree","session_id":local.metadata().id,"limit":2,"cursor":{"session_id":local.metadata().id,"revision":99,"offset":0}}));
    assert_eq!(client.response(5)["ok"], false);
    client.send(6, json!({"type":"select_session","session_id":null}));
    assert!(client.response(6)["result"]["session_id"].is_null());
    client.shutdown(7);
}
#[tokio::test]
async fn sdk_cli_json_and_rpc_produce_the_same_normalized_scripted_result() {
    use aicoder_core::{Agent, TurnExecutionContext};
    use aicoder_runtime::{
        ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings, resources::ResourceSettings,
    };
    use std::{collections::BTreeMap, sync::Arc};
    let root = tempfile::tempdir().unwrap();
    let (endpoint, listener) = listener();
    let server = std::thread::spawn(move || {
        for _ in 0..3 {
            let mut socket = accept(&listener);
            read_request(&mut socket);
            reply(
                &mut socket,
                json!({"role":"assistant","content":"same answer"}),
                "stop",
            );
        }
    });
    let env = Arc::new(BTreeMap::from([
        ("AICODER_API_BASE".into(), endpoint.clone()),
        ("AICODER_MODEL".into(), "test".into()),
        ("AICODER_PROXY".into(), "disabled".into()),
        ("AICODER_STREAM".into(), "false".into()),
        ("OPENAI_API_KEY".into(), "fixture-key".into()),
    ]));
    let runtime = Runtime::load_with_environment(
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
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
        env,
    )
    .unwrap();
    let agent = Agent::new(
        runtime
            .executor_builder(Arc::new(runtime.session_client().unwrap()))
            .unwrap()
            .build()
            .unwrap(),
        runtime.agent_config(),
    );
    let sdk = agent
        .run_outcome("parity", Arc::new(()), TurnExecutionContext::new())
        .await
        .into_result()
        .unwrap();
    let expected = json!({"text":sdk.execution_result.final_message.text_content(),"rounds":sdk.execution_result.progress.rounds,"usage":sdk.execution_result.progress.usage});
    let cli = base_command(root.path(), &endpoint, false)
        .args(["--json", "--prompt", "parity"])
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let frames: Vec<Value> = String::from_utf8(cli.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let text = frames
        .iter()
        .filter(|v| v["event"]["type"] == "content_chunk")
        .map(|v| v["event"]["delta"].as_str().unwrap())
        .collect::<String>();
    let terminal = frames
        .iter()
        .find(|v| v["event"]["type"] == "agent_completed")
        .unwrap();
    assert_eq!(
        json!({"text":text,"rounds":terminal["event"]["rounds"],"usage":terminal["event"]["usage"]}),
        expected
    );
    let mut client = Client::new(command(root.path(), &endpoint));
    client.send(1, json!({"type":"prompt","text":"parity"}));
    client.response(1);
    let result = client.finish_run();
    let text = client
        .frames
        .iter()
        .filter(|v| v["type"] == "event" && v["event"]["event"]["type"] == "content_chunk")
        .map(|v| v["event"]["event"]["delta"].as_str().unwrap())
        .collect::<String>();
    assert_eq!(
        json!({"text":text,"rounds":result["rounds"],"usage":result["usage"]}),
        expected
    );
    assert_eq!(result["status"], "completed");
    client.shutdown(2);
    assert_eq!(client.validate_traces(), 1);
    server.join().unwrap();
}
#[test]
fn rpc_fatal_input_limits_and_broken_stdout_cannot_leave_the_host_running() {
    let root = tempfile::tempdir().unwrap();
    for payload in [vec![b'x'; 1024 * 1024 + 1], vec![0xff, b'\n']] {
        let mut c = command(root.path(), "http://127.0.0.1:1/v1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = c.stdin.take().unwrap().write_all(&payload);
        let output = c.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let mut c = command(root.path(), "http://127.0.0.1:1/v1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(c.stdout.take());
    c.stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"version\":1,\"id\":1,\"command\":{\"type\":\"initialize\"}}\n")
        .unwrap();
    let start = Instant::now();
    loop {
        if let Some(status) = c.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = c.kill();
            panic!("closed stdout kept host running")
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn rpc_transport_failure_during_prepared_approval_awaits_journal_cleanup() {
    use aicoder_core::session::SessionRepository;
    let root = tempfile::tempdir().unwrap();
    let (endpoint, listener) = listener();
    let server = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        read_request(&mut socket);
        reply(
            &mut socket,
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"pending-write","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"not-committed.txt\",\"content\":\"never\"}"}}]}),
            "tool_calls",
        );
    });
    let mut child = configured_command(root.path(), &endpoint, true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"{\"version\":1,\"id\":1,\"command\":{\"type\":\"prompt\",\"text\":\"pending mutation\"}}\n").unwrap();
    let stdout = child.stdout.take().unwrap();
    let (seen, ready) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let frame: Value = serde_json::from_str(&line.unwrap()).unwrap();
            if frame["type"] == "event"
                && frame["event"]["event"]["type"] == "run_approval_requested"
            {
                let _ = seen.send(());
                break; // Close the actual pipe while the owner still awaits approval.
            }
        }
    });
    if ready.recv_timeout(Duration::from_secs(5)).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("prepared approval deadline");
    }
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"version\":1,\"id\":2,\"command\":{\"type\":\"initialize\"}}\n")
        .unwrap();
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("transport cleanup abandoned the execution owner")
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!root.path().join("not-committed.txt").exists());
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
    server.join().unwrap();
}
