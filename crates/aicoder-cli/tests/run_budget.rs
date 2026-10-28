use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn time_budget_exits_while_stdin_approval_remains_unanswered() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let n = socket.read(&mut buffer).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
            if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let body = "data: {\"id\":\"test\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"type\":\"function\",\"function\":{\"name\":\"write_file\",\"arguments\":\"{\\\"path\\\":\\\"never.txt\\\",\\\"content\\\":\\\"never\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n";
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    });
    let workspace = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aicoder"))
        .env_clear()
        .env("AICODER_HOME", workspace.path().join("state"))
        .args([
            "--no-config",
            "--no-session",
            "--prompt",
            "test",
            "--time-budget-ms",
            "1000",
            "--workspace",
        ])
        .arg(workspace.path())
        .env_remove("AICODER_API_KEY")
        .env_remove("AICODER_API_KEY_ENV")
        .env_remove("AICODER_API_BASE")
        .env("AICODER_PROXY", "disabled")
        .env("AICODER_MODEL", "test")
        .env("AICODER_COMPATIBILITY", "generic")
        .env("AICODER_MAX_RETRIES", "0")
        .env("AICODER_HTTP_TIMEOUT", "2")
        .env("AICODER_CONNECT_TIMEOUT", "1")
        .env("AICODER_MAX_RESPONSE_BYTES", "1048576")
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_API_BASE", format!("http://{address}/v1"))
        .env("OPENAI_MODEL", "test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("CLI did not exit after its run budget");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // Keep stdin open throughout the wait: EOF must not resolve the approval on behalf of cancellation.
    drop(child.stdin.take());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    server.join().unwrap();
    assert!(!status.success());
    assert!(
        stderr.contains("允许执行"),
        "approval was never reached: {stderr}"
    );
    assert!(
        stderr.contains("execution time"),
        "wrong stop reason: {stderr}"
    );
    assert!(!workspace.path().join("never.txt").exists());
}

#[test]
fn system_proxy_environment_is_used_by_cli_auto_mode() {
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin_address = origin.local_addr().unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    proxy.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let started = Instant::now();
        let (mut socket, _) = loop {
            match proxy.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        started.elapsed() < Duration::from_secs(4),
                        "client bypassed configured system proxy"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("proxy accept: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let n = socket.read(&mut buffer).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
            if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let body = "data: {\"id\":\"test\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"proxy response\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        String::from_utf8(request).unwrap()
    });
    let workspace = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aicoder"))
        .env_clear()
        .env("AICODER_HOME", workspace.path().join("state"))
        .args([
            "--no-config",
            "--no-session",
            "--prompt",
            "test",
            "--time-budget-ms",
            "2000",
            "--workspace",
        ])
        .arg(workspace.path())
        .env_remove("AICODER_API_KEY_ENV")
        .env("AICODER_API_KEY", "test-key")
        .env("AICODER_API_BASE", format!("http://{origin_address}/v1"))
        .env("AICODER_MODEL", "test")
        .env("AICODER_COMPATIBILITY", "generic")
        .env("AICODER_PROXY", "auto")
        .env("AICODER_HTTP_TIMEOUT", "2")
        .env("AICODER_CONNECT_TIMEOUT", "1")
        .env("AICODER_MAX_RETRIES", "0")
        .env("AICODER_MAX_RESPONSE_BYTES", "1048576")
        .env("HTTP_PROXY", format!("http://{proxy_address}"))
        .env("http_proxy", format!("http://{proxy_address}"))
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("CLI proxy request hung");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "proxy request failed: {stderr}");
    let request = server.join().unwrap();
    assert!(request.starts_with(&format!(
        "POST http://{origin_address}/v1/chat/completions HTTP/1.1"
    )));
    drop(origin);
}
