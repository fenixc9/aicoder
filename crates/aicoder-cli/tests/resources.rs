use std::{fs, process::Command};
#[test]
fn cli_inspects_sources_and_expands_prompts_without_credentials_or_session_writes() {
    let workspace = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    fs::write(
        workspace.path().join("AGENTS.md"),
        "RULE_BODY_NOT_INSPECTED",
    )
    .unwrap();
    fs::create_dir_all(workspace.path().join(".aicoder/prompts")).unwrap();
    fs::write(
        workspace.path().join(".aicoder/prompts/review.md"),
        "Review $1 with $@",
    )
    .unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_aicoder"))
            .current_dir(workspace.path())
            .env_clear()
            .env("AICODER_HOME", global.path())
            .args(args)
            .output()
            .unwrap()
    };
    let output = invoke(&["--no-parent-context", "resources"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(manifest["files"].as_array().unwrap().len(), 2);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("RULE_BODY_NOT_INSPECTED"));
    let expanded = invoke(&[
        "--no-parent-context",
        "expand-prompt",
        "/review 'two words'",
    ]);
    assert!(
        expanded.status.success(),
        "{}",
        String::from_utf8_lossy(&expanded.stderr)
    );
    let prompt: serde_json::Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert!(
        prompt["text"]
            .as_str()
            .unwrap()
            .contains("Review two words with two words")
    );
    assert!(!global.path().join("sessions").exists());
}

#[test]
fn actual_cli_request_contains_scoped_rules_and_expanded_template_input() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::{Duration, Instant},
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let started = Instant::now();
        let (mut socket, _) = loop {
            match listener.accept() {
                Ok(pair) => break pair,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        started.elapsed() < Duration::from_secs(4),
                        "CLI did not request the fixture"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let body = loop {
            let size = socket.read(&mut buffer).unwrap();
            assert!(size > 0);
            bytes.extend_from_slice(&buffer[..size]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break serde_json::from_slice::<serde_json::Value>(
                        &bytes[end + 4..end + 4 + length],
                    )
                    .unwrap();
                }
            }
        };
        let response = r#"{"id":"test","object":"chat.completion","created":1,"model":"test","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
        body
    });
    let workspace = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("AGENTS.md"), "CLI_ACTUAL_RULE").unwrap();
    fs::create_dir_all(workspace.path().join(".aicoder/prompts")).unwrap();
    fs::write(
        workspace.path().join(".aicoder/prompts/review.md"),
        "Review argument $1",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aicoder"))
        .current_dir(workspace.path())
        .env_clear()
        .env("AICODER_HOME", global.path())
        .env("OPENAI_API_KEY", "test-only")
        .env("AICODER_API_BASE", format!("http://{address}/v1"))
        .env("AICODER_MODEL", "test")
        .env("AICODER_PROXY", "disabled")
        .env("AICODER_STREAM", "false")
        .args([
            "--no-session",
            "--no-parent-context",
            "--prompt",
            "/review source.rs",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = server.join().unwrap();
    let messages = request["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message| {
        message["role"] == "system"
            && message["content"]
                .as_str()
                .unwrap_or_default()
                .contains("CLI_ACTUAL_RULE")
    }));
    assert!(
        messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Review argument source.rs")
    );
}
