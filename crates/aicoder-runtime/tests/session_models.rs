use aicoder_core::{model::*, session::*, *};
use aicoder_providers::{
    BearerToken, ChatClient, ClientConfig, CompatibilityProfile, CredentialProvider, ProxyMode,
};
use aicoder_runtime::SessionModelClient;
use async_trait::async_trait;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Credentials(Arc<AtomicUsize>);
#[async_trait]
impl CredentialProvider for Credentials {
    async fn bearer_token(&self) -> Result<BearerToken, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        BearerToken::new("fixture-token")
    }
}
#[tokio::test]
async fn model_selection_reuses_the_service_route_and_reads_credentials_only_for_generation() {
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
                    assert!(started.elapsed() < Duration::from_secs(4));
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("{error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let (headers, body) = loop {
            let n = socket.read(&mut buffer).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break (
                        headers.to_string(),
                        serde_json::from_slice::<serde_json::Value>(
                            &bytes[end + 4..end + 4 + length],
                        )
                        .unwrap(),
                    );
                }
            }
        };
        assert!(headers.starts_with("POST /v1/chat/completions "));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-token")
        );
        assert_eq!(body["model"], "selected");
        let response = r#"{"id":"fixture","object":"chat.completion","created":1,"model":"selected","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}]}"#;
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
    });
    let reads = Arc::new(AtomicUsize::new(0));
    let client = ChatClient::with_credentials(
        ClientConfig {
            provider_id: "host".into(),
            model: "default".into(),
            base_url: format!("http://{address}/v1"),
            proxy: ProxyMode::Disabled,
            ..Default::default()
        },
        Arc::new(Credentials(reads.clone())),
    )
    .unwrap();
    assert!(client.model_spec("selected").is_err());
    let client = SessionModelClient::new(client);
    assert_eq!(client.model_spec("selected").unwrap().provider, "host");
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let mut request = ModelRequest::new("selected", vec![Message::text(MessageRole::User, "test")]);
    request.stream = false;
    let response = client
        .generate(
            request,
            Arc::new(|_: ModelEvent| {}),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(response.message.text_content(), "done");
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    server.join().unwrap();
}
#[tokio::test]
async fn registered_metadata_and_adapter_restrictions_survive_profile_model_selection() {
    let reads = Arc::new(AtomicUsize::new(0));
    let mut client = ChatClient::with_credentials(
        ClientConfig {
            provider_id: "host".into(),
            model: "default".into(),
            proxy: ProxyMode::Disabled,
            ..Default::default()
        },
        Arc::new(Credentials(reads.clone())),
    )
    .unwrap();
    let mut spec = ModelSpec::new("host", "known");
    spec.capabilities.images = true;
    client.register_model(spec).unwrap();
    let client = SessionModelClient::new(client);
    assert!(client.model_spec("known").unwrap().capabilities.images);
    assert!(
        !client
            .model_spec("vision-looking-name")
            .unwrap()
            .capabilities
            .images
    );
    let workspace = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append(
        &mut session,
        Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ProviderData {
                provider: "host".into(),
                data: json!({"opaque":"keep private"}),
            }],
        },
    )
    .unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(client)
            .workspace(workspace.path())
            .build()
            .unwrap(),
        AgentConfig::new("default"),
    );
    assert!(
        agent
            .switch_session_model(
                &repo,
                &session.metadata().id,
                "selected",
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert_eq!(
        repo.open(&session.metadata().id).unwrap().metadata(),
        session.metadata()
    );
    let client = SessionModelClient::new(
        ChatClient::with_credentials(
            ClientConfig {
                provider_id: "host".into(),
                model: "default".into(),
                compatibility: CompatibilityProfile::DeepseekChat,
                proxy: ProxyMode::Disabled,
                ..Default::default()
            },
            Arc::new(Credentials(reads.clone())),
        )
        .unwrap(),
    );
    let mut request = ModelRequest::new("selected", vec![Message::text(MessageRole::User, "test")]);
    request.options.response_format = Some(ResponseFormat::JsonSchema {
        name: "result".into(),
        schema: json!({"type":"object"}),
        strict: true,
    });
    assert!(client.validate_request(&request).is_err());
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}
