use aicoder_core::{extensions::*, model::*, session::*, *};
use aicoder_providers::transport::*;
use aicoder_runtime::{resources::ResourceSettings, *};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
struct View;
#[async_trait]
impl RequestHook for View {
    async fn request(
        &self,
        input: RequestHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<RequestEdit>> {
        let mut options = input.request.options.clone();
        options.temperature = Some(0.25);
        options.seed = Some(42);
        options.max_output_tokens = Some(32);
        Ok(Some(RequestEdit {
            messages: Some(RequestMessages {
                messages: vec![
                    Message::text(MessageRole::System, "REQUEST_ONLY_SYSTEM"),
                    Message::text(MessageRole::User, "filtered request"),
                ],
                system_prefix_len: 1,
            }),
            tools: Some(vec![]),
            tool_choice: Some(ModelToolChoice::Auto),
            options: Some(options),
        }))
    }
}
async fn read(listener: &TcpListener) -> Result<(tokio::net::TcpStream, Value)> {
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept()).await??;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    let end = loop {
        let n = socket.read(&mut chunk).await?;
        ensure!(n > 0, "truncated request");
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            break i + 4;
        }
        ensure!(bytes.len() < 16384, "oversized headers");
    };
    let headers = std::str::from_utf8(&bytes[..end])?.to_owned();
    ensure!(
        headers.starts_with("POST /v1/chat/completions "),
        "wrong route"
    );
    ensure!(
        headers
            .to_lowercase()
            .contains("authorization: bearer fixture-key"),
        "wrong credential reference"
    );
    let size = headers
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>())
        })
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("missing content length"))?;
    ensure!(size < 65536, "oversized fixture request");
    while bytes.len() < end + size {
        let n = socket.read(&mut chunk).await?;
        ensure!(n > 0, "truncated body");
        bytes.extend_from_slice(&chunk[..n]);
    }
    let trace = headers
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case("x-transport-trace")
                .then(|| v.trim().to_string())
        })
        .ok_or_else(|| anyhow::anyhow!("transport trace header missing"))?;
    let payload: Value = serde_json::from_slice(&bytes[end..end + size])?;
    ensure!(
        payload["metadata"]["trace"] == trace,
        "headers/payload identity differs"
    );
    Ok((socket, payload))
}
/// Actual compatible HTTP adapter and JSONL sessions, loopback only, with fake credentials.
pub async fn run(stream: bool) -> Result<Value> {
    let root = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut socket, request) = read(&listener).await?;
        ensure!(
            request["model"] == "fixture"
                && request["temperature"] == 0.25
                && request["seed"] == 42
                && request["max_tokens"] == 32,
            "effective options were not sent"
        );
        ensure!(
            request["messages"][0]["content"] == "REQUEST_ONLY_SYSTEM"
                && request["messages"][1]["content"] == "filtered request",
            "effective view was not sent"
        );
        ensure!(
            request
                .get("tools")
                .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty)),
            "empty tool subset was refilled"
        );
        ensure!(request["stream"] == stream, "stream mode changed");
        let (body, mime) = if stream {
            (
                format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":"fixture","object":"chat.completion.chunk","created":1,"model":"fixture","choices":[{"index":0,"delta":{"role":"assistant","content":"done"},"finish_reason":"stop"}]}),
                    json!({"id":"fixture","object":"chat.completion.chunk","created":1,"model":"fixture","choices":[],"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}})
                ),
                "text/event-stream",
            )
        } else {
            (json!({"id":"fixture","object":"chat.completion","created":1,"model":"fixture","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}}).to_string(),"application/json")
        };
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nX-Public: observed\r\nX-Echo: fixture-key\r\nSet-Cookie: fixture-cookie\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await?;
        Ok::<_, anyhow::Error>(())
    });
    let settings = state.path().join("settings.json");
    std::fs::write(&settings,json!({"version":1,"profiles":{"fixture":{"provider_id":"fixture","base_url":format!("http://{address}/v1"),"model":"fixture","api_key_env":"FIXTURE_KEY","proxy":"disabled","models":[{"provider":"fixture","id":"fixture","context_tokens":65536,"max_output_tokens":128,"capabilities":{"tools":true,"temperature":true,"seed":true}}]}},"settings":{"provider_profile":"fixture","execution":{"stream":stream}}}).to_string())?;
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
            session_dir: Setting::Value(state.path().join("sessions")),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: true,
            user_file: Some(settings),
            project_file: None,
        },
        Arc::new(BTreeMap::from([(
            "FIXTURE_KEY".into(),
            "fixture-key".into(),
        )])),
    )?;
    let mut extension = Extension::new("example.request-view", "1")?;
    extension.request_hook(
        HookSpec::new("view"),
        RequestHookScope::SystemAndConversation,
        Arc::new(View),
    )?;
    let mut registry = ExtensionRegistry::new();
    registry.install(extension)?;
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = TransportHooks::new();
    hooks.allow_payload_field("metadata")?;
    hooks.register(
        "example.transport",
        HookSpec::new("trace"),
        Arc::new(Trace(trace.clone())),
    )?;
    let client = runtime.client()?.with_transport_hooks(hooks);
    let inspector = client.for_model("fixture")?;
    let agent = Agent::new(
        runtime
            .executor_builder_with_extensions(
                Arc::new(SessionModelClient::new(client)),
                Arc::new(registry.finish()),
            )?
            .build()?,
        runtime.agent_config(),
    );
    let repo = runtime.sessions()?;
    let session = repo.create(root.path())?;
    let turn = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "CANONICAL_USER_INPUT",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    server.await??;
    ensure!(
        turn.execution_result.usage.total_tokens == Some(11),
        "usage was double counted or lost"
    );
    let saved = repo.open(&session.metadata().id)?;
    let encoded = serde_json::to_string(saved.entries())?;
    ensure!(
        encoded.contains("CANONICAL_USER_INPUT")
            && !encoded.contains("REQUEST_ONLY_SYSTEM")
            && !encoded.contains("filtered request"),
        "projected messages became history"
    );
    ensure!(!encoded.contains("fixture-key"), "credential leaked");
    let views = saved
        .entries()
        .iter()
        .filter_map(|entry| match &entry.data {
            SessionEntryData::Extension { namespace, data }
                if namespace == REQUEST_VIEW_NAMESPACE =>
            {
                Some(data)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        views.len() == 2,
        "missing projection admission/final receipts"
    );
    let ready = views
        .iter()
        .find(|r| r["stage"] == "ready")
        .ok_or_else(|| anyhow::anyhow!("no final receipt"))?;
    ensure!(
        ready["effective_options"]["temperature"] == 0.25
            && ready["effective_options"]["seed"] == 42
            && ready["effective_options"]["max_output_tokens"] == 32,
        "effective scalar observations differ from HTTP"
    );
    ensure!(ready["tools"] == json!([]), "receipt lost empty tools");
    let phases = trace.lock().unwrap();
    ensure!(
        phases.starts_with(&["headers".into(), "payload".into(), "response".into()]),
        "transport stage order changed"
    );
    ensure!(
        inspector.transport_report().failures.is_empty(),
        "unexpected native transport failure"
    );
    Ok(
        json!({"stream":stream,"transport_stages":*phases,"transport_report_empty":true,"loopback_requests":1,"real_model_requests":0,"effective_options_sent":true,"empty_tools_preserved":true,"canonical_user_preserved":true,"request_only_context_not_persisted":true,"projection_receipts":views.len(),"usage":11}),
    )
}

struct Trace(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl TransportHook for Trace {
    async fn before_headers(
        &self,
        input: TransportHeadersInput<'_>,
        _: TransportCancellation,
    ) -> Result<BTreeMap<String, Option<String>>> {
        ensure!(
            !input.headers.contains_key("authorization"),
            "native callback received credential header"
        );
        self.0.lock().unwrap().push("headers".into());
        Ok(BTreeMap::from([(
            "x-transport-trace".into(),
            Some(input.request.id.clone()),
        )]))
    }
    async fn before_payload(
        &self,
        input: TransportPayloadInput<'_>,
        _: TransportCancellation,
    ) -> Result<Option<Value>> {
        self.0.lock().unwrap().push("payload".into());
        let mut payload = input.payload.clone();
        payload["metadata"] = json!({"trace":input.request.id});
        Ok(Some(payload))
    }
    async fn after_response(
        &self,
        input: TransportResponseInput<'_>,
        _: TransportCancellation,
    ) -> Result<()> {
        self.0.lock().unwrap().push("response".into());
        ensure!(
            input.status == 200
                && input
                    .headers
                    .get("x-public")
                    .is_some_and(|s| s == "observed"),
            "wrong response observation"
        );
        ensure!(
            !input.headers.contains_key("set-cookie")
                && !serde_json::to_string(input.headers)?.contains("fixture-key"),
            "private response header leaked"
        );
        Ok(())
    }
    async fn stream_event(
        &self,
        input: TransportStreamInput<'_>,
        _: TransportCancellation,
    ) -> Result<()> {
        ensure!(input.data.is_object(), "truncated stream observation");
        self.0.lock().unwrap().push("stream".into());
        Ok(())
    }
}
