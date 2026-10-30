use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
fn request(socket: &mut TcpStream) -> Value {
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let n = socket.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            let size = header
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
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
fn server(turns: usize) -> (String, std::thread::JoinHandle<Vec<Value>>) {
    server_case(turns, false)
}
fn server_case(turns: usize, tool: bool) -> (String, std::thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for i in 0..if turns == 99 { 1 } else { turns } {
            let start = Instant::now();
            let (mut socket, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(start.elapsed() < Duration::from_secs(5));
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            requests.push(request(&mut socket));
            let mut body = json!({"id":"fixture","object":"chat.completion","created":1,"model":"test","choices":[{"index":0,"message":{"role":"assistant","content":if turns==99 {"x".repeat(200000)}else{format!("answer{}",i+1)}},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}});
            if tool && i == 0 {
                body["choices"][0]["message"] = json!({"role":"assistant","content":null,"tool_calls":[{"id":"write","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"never.txt\",\"content\":\"never\"}"}}]});
                body["choices"][0]["finish_reason"] = json!("tool_calls");
            }
            let body = body.to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        requests
    });
    (address, worker)
}
fn command(workspace: &std::path::Path, address: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aicoder"));
    command
        .env_clear()
        .env("AICODER_HOME", workspace.join("state"))
        .env("AICODER_MODEL", "test")
        .env("AICODER_API_BASE", address)
        .env("OPENAI_API_KEY", "fixture-key")
        .env("AICODER_STREAM", "false")
        .env("AICODER_PROXY", "disabled")
        .env("AICODER_MAX_RETRIES", "0")
        .args([
            "--no-config",
            "--no-resources",
            "--no-session",
            "--workspace",
        ])
        .arg(workspace);
    command
}
fn invoke(mut command: Command, args: &[&str], input: &[u8]) -> Output {
    let mut child = command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut pipe = child.stdin.take().unwrap();
    let _ = pipe.write_all(input);
    drop(pipe);
    child.wait_with_output().unwrap()
}
#[test]
fn positional_and_stdin_text_modes_emit_only_the_current_final_text() {
    for args in [
        vec!["--print", "positional prompt"],
        vec!["--print", "--stdin"],
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let (address, server) = server(1);
        let output = invoke(
            command(workspace.path(), &address),
            &args,
            b"stdin prompt\nsecond line",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"answer1\n");
        let requests = server.join().unwrap();
        assert!(
            requests[0]["messages"]
                .to_string()
                .contains(if args.contains(&"--stdin") {
                    "stdin prompt"
                } else {
                    "positional prompt"
                })
        );
    }
}
#[test]
fn json_stdout_is_a_complete_sdk_trace_without_plaintext_summaries() {
    let workspace = tempfile::tempdir().unwrap();
    let (address, server) = server(1);
    let output = invoke(
        command(workspace.path(), &address),
        &["--json", "--prompt", "JSON case"],
        b"",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut validator = aicoder_core::events::EventTraceValidator::new();
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let frame: aicoder_core::AgentRawEventEnvelope = serde_json::from_str(line).unwrap();
        validator.observe(&frame).unwrap();
    }
    validator.finish().unwrap();
    server.join().unwrap();
}
#[test]
fn piped_repl_keeps_ephemeral_history_and_exit_never_becomes_a_prompt() {
    let workspace = tempfile::tempdir().unwrap();
    let (address, server) = server(2);
    let output = invoke(
        command(workspace.path(), &address),
        &["--repl", "--print"],
        b"first\nsecond\n/exit\nnever sent\n",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"answer1\nanswer2\n");
    let requests = server.join().unwrap();
    assert!(requests[1]["messages"].to_string().contains("answer1"));
    assert!(requests[1]["messages"].to_string().contains("first"));
    assert!(!workspace.path().join("state/sessions").exists());
}
#[test]
fn empty_oversized_and_conflicting_inputs_fail_without_contacting_a_provider() {
    let workspace = tempfile::tempdir().unwrap();
    for (args, input) in [
        (vec!["--stdin", "--print"], vec![]),
        (vec!["--stdin", "--print"], vec![b'x'; 1024 * 1024 + 1]),
        (vec!["--stdin", "--prompt", "conflict"], vec![]),
        (vec!["--json", "--print", "--prompt", "conflict"], vec![]),
    ] {
        let output = invoke(
            command(workspace.path(), "http://127.0.0.1:1/v1"),
            &args,
            &input,
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}
#[test]
fn ctrl_c_cancels_the_owner_and_json_trace_ends_before_exit_130() {
    let workspace = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    let (sent, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        request(&mut socket);
        sent.send(()).unwrap();
        let mut byte = [0];
        let _ = socket.read(&mut byte);
    });
    let mut child = command(workspace.path(), &address)
        .args(["--json", "--prompt", "cancel"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    received.recv_timeout(Duration::from_secs(4)).unwrap();
    unsafe {
        assert_eq!(libc::kill(child.id() as i32, libc::SIGINT), 0);
    }
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(4) {
            child.kill().unwrap();
            panic!("cancel did not stop CLI");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut validator = aicoder_core::events::EventTraceValidator::new();
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let frame = serde_json::from_str(line).unwrap();
        validator.observe(&frame).unwrap();
    }
    validator.finish().unwrap();
    worker.join().unwrap();
}

#[test]
fn piped_repl_prompt_lines_cannot_be_consumed_as_tool_authorization() {
    let workspace = tempfile::tempdir().unwrap();
    let (address, server) = server_case(3, true);
    let output = invoke(
        command(workspace.path(), &address),
        &["--repl", "--json"],
        b"first\ny\n/exit\n",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!workspace.path().join("never.txt").exists());
    let requests = server.join().unwrap();
    assert!(
        requests[1]["messages"]
            .to_string()
            .contains("approval_denied")
    );
    assert_eq!(
        requests[2]["messages"].as_array().unwrap().last().unwrap()["content"],
        "y\n"
    );
    let mut validator = None;
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let frame: aicoder_core::AgentRawEventEnvelope = serde_json::from_str(line).unwrap();
        if matches!(
            frame.event,
            aicoder_core::AgentRawEvent::AgentStarted { .. }
        ) {
            if let Some(old) = &validator {
                aicoder_core::events::EventTraceValidator::finish(old).unwrap();
            }
            validator = Some(aicoder_core::events::EventTraceValidator::new());
        }
        validator.as_mut().unwrap().observe(&frame).unwrap();
    }
    validator.unwrap().finish().unwrap();
}

#[test]
fn stalled_json_consumer_cannot_hold_the_execution_owner_or_global_stdout_lock() {
    let workspace = tempfile::tempdir().unwrap();
    let (address, server) = server(99);
    let mut child = command(workspace.path(), &address)
        .args(["--json", "--prompt", "large reply"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("stalled JSON sink held CLI open");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("incomplete"));
    server.join().unwrap();
}
