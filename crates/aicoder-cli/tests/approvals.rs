use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    time::Duration,
};
#[test]
fn cli_session_grants_are_explicit_and_once_or_denial_do_not_persist() {
    for (input, expected, prompts) in [
        ("s\n", Some("second"), 1),
        ("y\nn\n", Some("first"), 2),
        ("n\nn\n", None, 2),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            for index in 0..3 {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let (mut socket, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "CLI did not send expected request {index}"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if request.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let message = if index < 2 {
                    json!({"role":"assistant","tool_calls":[{"id":format!("call-{index}"),"type":"function","function":{"name":"write_file","arguments":json!({"path":"file.txt","content":if index==0{"first"}else{"second"}}).to_string()}}]})
                } else {
                    json!({"role":"assistant","content":"done"})
                };
                let body=json!({"id":format!("response-{index}"),"object":"chat.completion","created":1,"model":"test","choices":[{"index":0,"message":message,"finish_reason":if index<2{"tool_calls"}else{"stop"}}]}).to_string();
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_aicoder"))
            .env_clear()
            .env("AICODER_HOME", root.path())
            .env("AICODER_API_BASE", format!("http://{address}/v1"))
            .env("OPENAI_API_KEY", "fake-key")
            .env("AICODER_PROXY", "disabled")
            .env("AICODER_STREAM", "false")
            .env("AICODER_MAX_RETRIES", "0")
            .env("AICODER_HTTP_TIMEOUT", "3")
            .args([
                "--no-config",
                "--model",
                "test",
                "--no-resources",
                "--prompt",
                "write twice",
                "--time-budget-ms",
                "5000",
                "--workspace",
            ])
            .arg(workspace.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        server.join().unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("file.txt"))
                .ok()
                .as_deref(),
            expected
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.matches("允许执行?").count(), prompts);
        assert!(stderr.contains("工具 write_file 的所有参数"));
    }
}
