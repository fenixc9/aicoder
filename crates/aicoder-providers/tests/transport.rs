use aicoder_core::{TurnExecutionContext, extensions::HookSpec, model::*};
use aicoder_providers::{
    BearerToken, ChatClient, ClientConfig, CredentialProvider, ProxyMode, transport::*,
};
use anyhow::Result;
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
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
struct Response {
    status: &'static str,
    body: String,
    stream: bool,
    short: bool,
}
fn success() -> Value {
    json!({"id":"r","object":"chat.completion","created":1,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}})
}
fn chunk(text: &str, finish: bool) -> Value {
    json!({"id":"r","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"content":text},"finish_reason":if finish{json!("stop")}else{Value::Null}}],"future_field":"fixture-key"})
}
fn usage_chunk() -> Value {
    json!({"id":"r","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}})
}
fn sse(values: Vec<Value>) -> String {
    values
        .into_iter()
        .map(|v| format!("data: {v}\n\n"))
        .collect::<String>()
        + "data: [DONE]\n\n"
}
async fn server(
    responses: Vec<Response>,
) -> (String, tokio::task::JoinHandle<Vec<(String, Value)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut captured = Vec::new();
        for response in responses {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            let end = loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    break i + 4;
                }
                assert!(bytes.len() < 16384);
            };
            let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
            let size = headers
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            assert!(size < 131072);
            while bytes.len() < end + size {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
            }
            captured.push((
                headers,
                serde_json::from_slice(&bytes[end..end + size]).unwrap(),
            ));
            let length = response.body.len() + if response.short { 100 } else { 0 };
            let header = format!(
                "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {length}\r\nRetry-After: 0\r\nX-Echo: fixture-key\r\nSet-Cookie: private-cookie\r\nConnection: close\r\n\r\n",
                response.status,
                if response.stream {
                    "text/event-stream"
                } else {
                    "application/json"
                }
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(response.body.as_bytes()).await.unwrap();
        }
        captured
    });
    (format!("http://{address}/v1"), task)
}
fn config(url: String, retries: u32) -> ClientConfig {
    ClientConfig {
        base_url: url,
        provider_id: "fixture".into(),
        api_key: "fixture-key".into(),
        model: "m".into(),
        max_retries: retries,
        timeout: 10,
        proxy: ProxyMode::Disabled,
        ..Default::default()
    }
}
fn request(stream: bool) -> ModelRequest {
    let mut r = ModelRequest::new("m", vec![Message::text(MessageRole::User, "hello")]);
    r.stream = stream;
    r.options.max_output_tokens = Some(32);
    r
}
#[derive(Default)]
struct State {
    phases: Mutex<Vec<String>>,
    events: Mutex<Vec<Value>>,
    ids: Mutex<Vec<String>>,
}
struct Hook {
    label: &'static str,
    state: Arc<State>,
    fail: bool,
}
#[async_trait]
impl TransportHook for Hook {
    async fn before_headers(
        &self,
        input: TransportHeadersInput<'_>,
        _: TransportCancellation,
    ) -> Result<BTreeMap<String, Option<String>>> {
        assert!(!input.headers.contains_key("authorization"));
        self.state
            .phases
            .lock()
            .unwrap()
            .push(format!("headers:{}", self.label));
        self.state
            .ids
            .lock()
            .unwrap()
            .push(input.request.id.clone());
        Ok(BTreeMap::from([
            (format!("x-{}", self.label), Some(input.request.id.clone())),
            (
                "x-temporary".into(),
                if self.label == "first" {
                    Some("remove".into())
                } else {
                    None
                },
            ),
        ]))
    }
    async fn before_payload(
        &self,
        input: TransportPayloadInput<'_>,
        _: TransportCancellation,
    ) -> Result<Option<Value>> {
        self.state
            .phases
            .lock()
            .unwrap()
            .push(format!("payload:{}", self.label));
        let mut payload = input.payload.clone();
        payload["metadata"][self.label] = json!(input.request.id);
        Ok(Some(payload))
    }
    async fn after_response(
        &self,
        input: TransportResponseInput<'_>,
        _: TransportCancellation,
    ) -> Result<()> {
        self.state
            .phases
            .lock()
            .unwrap()
            .push(format!("response:{}:{}", self.label, input.attempt));
        assert!(!input.headers.contains_key("set-cookie"));
        assert!(!input.headers_complete);
        assert!(
            !serde_json::to_string(input.headers)
                .unwrap()
                .contains("fixture-key")
        );
        if self.fail {
            anyhow::bail!("private-native-error fixture-key");
        }
        Ok(())
    }
    async fn stream_event(
        &self,
        input: TransportStreamInput<'_>,
        _: TransportCancellation,
    ) -> Result<()> {
        self.state.events.lock().unwrap().push(input.data.clone());
        if input.data.get("future_field").is_some() {
            assert!(input.credentials_redacted);
            assert!(!input.data.to_string().contains("fixture-key"));
        }
        if self.fail {
            anyhow::bail!("private-native-error fixture-key");
        }
        Ok(())
    }
}
fn hooks(state: Arc<State>, fail: bool) -> TransportHooks {
    let mut h = TransportHooks::new();
    h.allow_payload_field("metadata").unwrap();
    for (label, priority) in [("second", 10), ("first", -1)] {
        h.register(
            "example",
            HookSpec {
                priority,
                ..HookSpec::new(label)
            },
            Arc::new(Hook {
                label,
                state: state.clone(),
                fail,
            }),
        )
        .unwrap();
    }
    h
}
#[tokio::test]
async fn typed_json_and_sse_requests_use_ordered_headers_and_payload_without_exposing_credentials()
{
    for stream in [false, true] {
        let body = if stream {
            sse(vec![chunk("done", true), usage_chunk()])
        } else {
            success().to_string()
        };
        let (url, server) = server(vec![Response {
            status: "200 OK",
            body,
            stream,
            short: false,
        }])
        .await;
        let state = Arc::new(State::default());
        let client = ChatClient::new(config(url, 1))
            .unwrap()
            .with_transport_hooks(hooks(state.clone(), false));
        let response = client
            .generate(request(stream), Arc::new(()), TurnExecutionContext::new())
            .await
            .unwrap();
        assert_eq!(response.usage.total_tokens, Some(11));
        let captured = server.await.unwrap();
        assert_eq!(captured.len(), 1);
        let (header, payload) = &captured[0];
        assert!(
            header
                .to_lowercase()
                .contains("authorization: bearer fixture-key")
        );
        assert!(header.to_lowercase().contains("x-first:"));
        assert!(!header.to_lowercase().contains("x-temporary:"));
        assert_eq!(payload["messages"][0]["content"], "hello");
        assert_eq!(payload["max_tokens"], 32);
        assert_eq!(payload["metadata"]["first"], payload["metadata"]["second"]);
        assert_eq!(
            &state.phases.lock().unwrap()[..4],
            &[
                "headers:first",
                "headers:second",
                "payload:first",
                "payload:second"
            ]
        );
        assert!(client.transport_report().failures.is_empty());
        if stream {
            assert_eq!(state.events.lock().unwrap().len(), 4);
        }
        assert_eq!(client.transport_hook_catalog().len(), 2);
        assert_eq!(
            client
                .for_model("m")
                .unwrap()
                .transport_hook_catalog()
                .len(),
            2
        );
    }
}
#[tokio::test]
async fn observer_errors_never_change_normalization_trigger_retry_or_lose_usage_and_reports_are_bounded()
 {
    let values = (0..140)
        .map(|_| chunk("x", false))
        .chain([chunk("", true), usage_chunk()])
        .collect();
    let (url, server) = server(vec![Response {
        status: "200 OK",
        body: sse(values),
        stream: true,
        short: false,
    }])
    .await;
    let state = Arc::new(State::default());
    let client = ChatClient::new(config(url, 3))
        .unwrap()
        .with_transport_hooks(hooks(state, true));
    let response = client
        .generate(request(true), Arc::new(()), TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(response.message.text_content(), "x".repeat(140));
    assert_eq!(response.usage.total_tokens, Some(11));
    assert_eq!(server.await.unwrap().len(), 1);
    let report = client.transport_report();
    assert_eq!(report.failures.len(), 128);
    assert!(report.dropped > 0);
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("private-native-error") && !encoded.contains("fixture-key"));
}
struct Bad(&'static str);
#[async_trait]
impl TransportHook for Bad {
    async fn before_headers(
        &self,
        _: TransportHeadersInput<'_>,
        _: TransportCancellation,
    ) -> Result<BTreeMap<String, Option<String>>> {
        match self.0 {
            "auth" => Ok(BTreeMap::from([(
                "Authorization".into(),
                Some("forged".into()),
            )])),
            "header" => Ok(BTreeMap::from([(
                "x-trace".into(),
                Some("bad\r\nvalue".into()),
            )])),
            "timeout" => {
                std::thread::sleep(Duration::from_millis(10));
                Ok(BTreeMap::new())
            }
            _ => Ok(BTreeMap::new()),
        }
    }
    async fn before_payload(
        &self,
        input: TransportPayloadInput<'_>,
        _: TransportCancellation,
    ) -> Result<Option<Value>> {
        let mut value = input.payload.clone();
        match self.0 {
            "model" => value["model"] = json!("forged"),
            "messages" => value["messages"] = json!([]),
            "max" => value["max_tokens"] = json!(99999),
            "unknown" => value["unapproved"] = json!(true),
            "oversize" => value["metadata"] = json!("x".repeat(65537)),
            _ => {}
        }
        Ok(Some(value))
    }
}
struct Credential(Arc<AtomicUsize>);
#[async_trait]
impl CredentialProvider for Credential {
    async fn bearer_token(&self) -> Result<BearerToken, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        BearerToken::new("fixture-key")
    }
}
#[tokio::test]
async fn invalid_mutations_fail_before_credentials_or_http_and_common_authority_fields_cannot_be_allowed()
 {
    for field in [
        "model",
        "MODEL",
        "messages",
        "max_tokens",
        "api_key",
        "endpoint",
    ] {
        assert!(TransportHooks::new().allow_payload_field(field).is_err());
    }
    for mode in [
        "auth", "header", "model", "messages", "max", "unknown", "oversize", "timeout",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let mut h = TransportHooks::new();
        h.allow_payload_field("metadata").unwrap();
        h.register(
            "bad",
            HookSpec {
                timeout: if mode == "timeout" {
                    Duration::from_millis(1)
                } else {
                    Duration::from_secs(2)
                },
                ..HookSpec::new("bad")
            },
            Arc::new(Bad(mode)),
        )
        .unwrap();
        let client = ChatClient::with_credentials(
            config(format!("http://{}/v1", listener.local_addr().unwrap()), 0),
            Arc::new(Credential(count.clone())),
        )
        .unwrap()
        .with_transport_hooks(h);
        let events = Arc::new(Events::default());
        assert!(
            client
                .generate(request(false), events.clone(), TurnExecutionContext::new())
                .await
                .is_err(),
            "{mode}"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(
            events
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e,ModelEvent::Usage{usage}if usage.total_tokens==Some(0)))
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
        assert!(!client.transport_report().failures.is_empty());
    }
}
#[tokio::test]
async fn retryable_status_reuses_the_prepared_plan_and_reports_each_actual_response() {
    let (url, server) = server(vec![
        Response {
            status: "429 Too Many Requests",
            body: "{}".into(),
            stream: false,
            short: false,
        },
        Response {
            status: "200 OK",
            body: success().to_string(),
            stream: false,
            short: false,
        },
    ])
    .await;
    let state = Arc::new(State::default());
    let client = ChatClient::new(config(url, 1))
        .unwrap()
        .with_transport_hooks(hooks(state.clone(), false));
    client
        .generate(request(false), Arc::new(()), TurnExecutionContext::new())
        .await
        .unwrap();
    let captured = server.await.unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].1, captured[1].1);
    assert_eq!(
        *state.phases.lock().unwrap(),
        vec![
            "headers:first",
            "headers:second",
            "payload:first",
            "payload:second",
            "response:first:1",
            "response:second:1",
            "response:first:2",
            "response:second:2"
        ]
    );
}
#[tokio::test]
async fn hooked_successful_headers_never_retry_an_incomplete_completed_response_body() {
    let (url, server) = server(vec![Response {
        status: "200 OK",
        body: "{".into(),
        stream: false,
        short: true,
    }])
    .await;
    let client = ChatClient::new(config(url, 2))
        .unwrap()
        .with_transport_hooks(hooks(Arc::new(State::default()), false));
    assert!(
        client
            .generate(request(false), Arc::new(()), TurnExecutionContext::new())
            .await
            .is_err()
    );
    assert_eq!(server.await.unwrap().len(), 1);
}
struct WaitUsage {
    started: Arc<tokio::sync::Notify>,
    cancelled: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl TransportHook for WaitUsage {
    async fn stream_event(
        &self,
        input: TransportStreamInput<'_>,
        c: TransportCancellation,
    ) -> Result<()> {
        if input.data.get("usage").is_some() {
            let ended = self.cancelled.clone();
            tokio::spawn(async move {
                c.cancelled().await;
                ended.notify_one();
            });
            self.started.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<ModelEvent>>);
impl ModelEventSink for Events {
    fn emit(&self, event: ModelEvent) {
        self.0.lock().unwrap().push(event);
    }
}
#[tokio::test]
async fn cancellation_during_raw_usage_observation_retains_known_usage_and_expires_children() {
    let (url, server) = server(vec![Response {
        status: "200 OK",
        body: sse(vec![chunk("done", true), usage_chunk()]),
        stream: true,
        short: false,
    }])
    .await;
    let started = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let mut hooks = TransportHooks::new();
    hooks
        .register(
            "usage",
            HookSpec::new("wait"),
            Arc::new(WaitUsage {
                started: started.clone(),
                cancelled: cancelled.clone(),
            }),
        )
        .unwrap();
    let client = ChatClient::new(config(url, 0))
        .unwrap()
        .with_transport_hooks(hooks);
    let events = Arc::new(Events::default());
    let sink = events.clone();
    let context = TurnExecutionContext::new();
    let worker = context.clone();
    let task = tokio::spawn(async move { client.generate(request(true), sink, worker).await });
    started.notified().await;
    context.cancel("cancel observing usage");
    let result = task.await.unwrap();
    assert!(matches!(result,Err(ref e)if e.kind==ProviderErrorKind::Cancelled));
    tokio::time::timeout(Duration::from_secs(1), cancelled.notified())
        .await
        .unwrap();
    let usages = events
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| {
            if let ModelEvent::Usage { usage } = e {
                Some(usage.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].total_tokens, Some(11));
    assert_eq!(server.await.unwrap().len(), 1);
}
#[tokio::test]
async fn oversized_stream_events_are_omitted_explicitly_without_truncating_model_content() {
    let large = "x".repeat(MAX_TRANSPORT_EVENT_BYTES + 1024);
    let (url, server) = server(vec![Response {
        status: "200 OK",
        body: sse(vec![chunk(&large, true), usage_chunk()]),
        stream: true,
        short: false,
    }])
    .await;
    let state = Arc::new(State::default());
    let client = ChatClient::new(config(url, 0))
        .unwrap()
        .with_transport_hooks(hooks(state.clone(), false));
    let response = client
        .generate(request(true), Arc::new(()), TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(response.message.text_content(), large);
    assert_eq!(state.events.lock().unwrap().len(), 2);
    assert!(
        client
            .transport_report()
            .failures
            .iter()
            .any(|r| r.failure == "event_too_large")
    );
    server.await.unwrap();
}
#[tokio::test]
async fn public_driver_sessions_share_guards_deadlines_and_owned_cancellation() {
    let mut h = TransportHooks::new();
    h.allow_payload_field("metadata").unwrap();
    h.register("driver", HookSpec::new("pass"), Arc::new(Bad("pass")))
        .unwrap();
    let body = json!({"model":"m","messages":[],"stream":false});
    let session =
        TransportSession::start("custom", &body, Arc::new(h), Duration::from_secs(1)).unwrap();
    let (value, _) = session.prepare(&body, BTreeMap::new()).await.unwrap();
    assert_eq!(value, body);
    session.cancel("driver cancelled");
    assert!(session.ensure_active().is_err());
    assert!(session.report().failures.is_empty());
}

struct Blocking;
#[async_trait]
impl TransportHook for Blocking {
    async fn before_headers(
        &self,
        _: TransportHeadersInput<'_>,
        _: TransportCancellation,
    ) -> Result<BTreeMap<String, Option<String>>> {
        std::thread::sleep(Duration::from_millis(60));
        Ok(BTreeMap::new())
    }
}
#[tokio::test]
async fn parent_deadline_after_native_preparation_prevents_starting_an_http_attempt() {
    use aicoder_core::{RunBudget, RunOutcome, RunStop, TurnExecutionConfig, TurnExecutor};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut h = TransportHooks::new();
    h.register("blocking", HookSpec::new("work"), Arc::new(Blocking))
        .unwrap();
    let client = ChatClient::new(config(
        format!("http://{}/v1", listener.local_addr().unwrap()),
        0,
    ))
    .unwrap()
    .with_transport_hooks(h);
    let outcome = TurnExecutor::builder(client)
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_duration: Some(Duration::from_millis(30)),
                ..Default::default()
            },
            stream: false,
            ..Default::default()
        })
        .build()
        .unwrap()
        .run_outcome(request(false), Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome,RunOutcome::Interrupted(ref i)if matches!(i.stop,RunStop::Budget(_))));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn observation_representation_failure_cannot_change_typed_stream_normalization() {
    let mut event = chunk("done", true).to_string();
    event.pop();
    event.push_str(",\"unknown_number\":1e400}");
    // The wire codec ignores this extension; Value's finite-number representation rejects it.
    assert!(serde_json::from_str::<aicoder_providers::wire::StreamChunk>(&event).is_ok());
    assert!(serde_json::from_str::<Value>(&event).is_err());
    let body = format!(
        "data: {event}\n\ndata: {}\n\ndata: [DONE]\n\n",
        usage_chunk()
    );
    let (url, server) = server(vec![Response {
        status: "200 OK",
        body,
        stream: true,
        short: false,
    }])
    .await;
    let client = ChatClient::new(config(url, 0))
        .unwrap()
        .with_transport_hooks(hooks(Arc::new(State::default()), false));
    let response = client
        .generate(request(true), Arc::new(()), TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(response.message.text_content(), "done");
    assert_eq!(response.usage.total_tokens, Some(11));
    assert!(
        client
            .transport_report()
            .failures
            .iter()
            .any(|r| r.failure == "event_unparseable")
    );
    server.await.unwrap();
}

struct PanicObserver;
#[async_trait]
impl TransportHook for PanicObserver {
    async fn after_response(
        &self,
        _: TransportResponseInput<'_>,
        _: TransportCancellation,
    ) -> Result<()> {
        panic!("fixture native panic")
    }
}
#[tokio::test]
async fn observer_panic_is_reported_without_repeating_a_successful_request() {
    let (url, server) = server(vec![Response {
        status: "200 OK",
        body: success().to_string(),
        stream: false,
        short: false,
    }])
    .await;
    let mut h = TransportHooks::new();
    h.register("observer", HookSpec::new("panic"), Arc::new(PanicObserver))
        .unwrap();
    let client = ChatClient::new(config(url, 2))
        .unwrap()
        .with_transport_hooks(h);
    let response = client
        .generate(request(false), Arc::new(()), TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(response.usage.total_tokens, Some(11));
    assert_eq!(server.await.unwrap().len(), 1);
    assert!(
        client
            .transport_report()
            .failures
            .iter()
            .any(|r| r.failure == "panicked")
    );
}

#[cfg(feature = "auxiliary-models")]
#[path = "support/transport_auxiliary.rs"]
mod auxiliary;
