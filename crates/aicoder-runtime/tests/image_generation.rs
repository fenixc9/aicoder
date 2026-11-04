#![cfg(feature = "auxiliary-models")]
use aicoder_core::{artifacts::*, auxiliary::*, model::*, session::*, tools::*, *};
use aicoder_runtime::{resources::ResourceSettings, *};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
struct Model {
    calls: Mutex<Vec<ModelRequest>>,
    program: Option<String>,
}
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.tools = true;
        spec.capabilities.images = true;
        spec.context_tokens = Some(65536);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        _: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let first = !request.messages.iter().any(|m| m.role == MessageRole::Tool);
        self.calls.lock().unwrap().push(request.clone());
        let (name, args) = if let Some(code) = &self.program {
            ("codemode", json!({"code":code}))
        } else {
            (
                "model_generate_images",
                json!({"model":{"provider":"painter","id":"native"},"context":{"input":[{"type":"text","text":"Paint a fox"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":PNG}}]}}),
            )
        };
        Ok(ModelResponse {
            id: None,
            model: request.model,
            message: if first {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "task".into(),
                            name: name.into(),
                            arguments: args,
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::Assistant, "done")
            },
            stop_reason: if first {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage {
                input_tokens: Some(5),
                output_tokens: Some(2),
                total_tokens: Some(7),
                cached_input_tokens: Some(0),
                cache_write_tokens: Some(0),
            },
        })
    }
}
async fn request(listener: &TcpListener) -> (tokio::net::TcpStream, Value) {
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    let end = loop {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(at) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            break at + 4;
        }
        assert!(bytes.len() < 16384);
    };
    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
    assert!(headers.starts_with("POST /v1/chat/completions "));
    assert!(
        headers
            .to_lowercase()
            .contains("authorization: bearer fixture-painter")
    );
    let size = headers
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    assert!(size < 2 * 1024 * 1024);
    while bytes.len() < end + size {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
    }
    (
        socket,
        serde_json::from_slice(&bytes[end..end + size]).unwrap(),
    )
}
async fn respond(mut socket: tokio::net::TcpStream, body: Value) {
    let body = body.to_string();
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
}
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAIAAAB7QOjdAAAAD0lEQVR4nGP4z8DA8J8BAAf/Af8Bf4mnAAAAAElFTkSuQmCC";
fn runtime_with_storage(
    root: &std::path::Path,
    state: &std::path::Path,
    address: std::net::SocketAddr,
    program: bool,
    storage: bool,
) -> Runtime {
    let file = state.join("settings.json");
    fs::write(&file,json!({"version":1,"profiles":{"native":{"provider_id":"painter","base_url":format!("http://{address}/v1"),"api_key_env":"PAINTER_KEY","proxy":"disabled","image_models":[{"provider":"painter","id":"native","name":"Image fixture","images":true,"text_output":true,"context_tokens":65536}]}},"settings":{"code_mode":{"enabled":program,"only":program,"model_profiles":["native"]}}}).to_string()).unwrap();
    Runtime::load_with_environment(
        root,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: if storage {
                Setting::Value(state.join("sessions"))
            } else {
                Setting::Clear
            },
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: true,
            user_file: Some(file),
            project_file: None,
        },
        Arc::new(BTreeMap::from([(
            "PAINTER_KEY".into(),
            "fixture-painter".into(),
        )])),
    )
    .unwrap()
}
fn runtime(
    root: &std::path::Path,
    state: &std::path::Path,
    address: std::net::SocketAddr,
    program: bool,
) -> Runtime {
    runtime_with_storage(root, state, address, program, true)
}
fn response(url: &str) -> Value {
    json!({"choices":[{"finish_reason":"stop","message":{"content":"Painted","images":[{"image_url":{"url":url}},{"image_url":format!("data:image/png;base64,{PNG}")}]}}],"usage":{"prompt_tokens":9,"completion_tokens":3,"total_tokens":12}})
}
fn model(program: Option<String>) -> Arc<Model> {
    Arc::new(Model {
        calls: Mutex::new(vec![]),
        program,
    })
}
#[tokio::test]
async fn native_image_generation_captures_raw_artifacts_and_meters_valid_and_invalid_output() {
    for valid in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, body) = request(&listener).await;
            assert_eq!(body["model"], "native");
            assert_eq!(body["modalities"], json!(["image", "text"]));
            assert_eq!(body["stream"], false);
            assert_eq!(
                body["messages"][0]["content"][1]["image_url"]["url"],
                format!("data:image/png;base64,{PNG}")
            );
            respond(
                socket,
                response(&if valid {
                    format!("data:image/png;base64,{PNG}")
                } else {
                    "https://must-not-fetch.invalid/private".into()
                }),
            )
            .await;
        });
        let runtime = runtime(root.path(), state.path(), address, false);
        let provider = model(None);
        let store = Arc::new(MemoryArtifactStore::default());
        let agent = Agent::new(
            runtime
                .executor_builder(provider.clone())
                .unwrap()
                .artifact_store(store.clone())
                .approval(AllowAllApproval)
                .build()
                .unwrap(),
            AgentConfig::new("fixture"),
        );
        let repo = runtime.sessions().unwrap();
        let session = repo.create(root.path()).unwrap();
        let turn = agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "paint",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
        server.await.unwrap();
        let progress = &turn.execution_result;
        assert_eq!(progress.usage.total_tokens, Some(26));
        assert_eq!(
            progress.primary_usage.as_ref().unwrap().total_tokens,
            Some(14)
        );
        assert_eq!(progress.auxiliary.len(), 1);
        assert_eq!(progress.auxiliary[0].delivery_complete, valid);
        assert_eq!(
            progress.auxiliary[0].status,
            if valid { "completed" } else { "error" }
        );
        let result = progress
            .messages
            .iter()
            .find_map(Message::tool_result)
            .unwrap();
        assert_eq!(result.2, !valid, "{}", result.1);
        if valid {
            let output = &result.1["output"]["output"];
            assert_eq!(output[0]["text"], "Painted");
            for image in output.as_array().unwrap().iter().skip(1) {
                let reference: ArtifactRef =
                    serde_json::from_value(image["artifact"].clone()).unwrap();
                let page = store.read(&reference.id, 0, 4096).await.unwrap();
                assert!(page.bytes.starts_with(b"\x89PNG"));
                assert!(reference.complete);
            }
        } else {
            assert_eq!(result.1["error"]["details"]["usage"]["total_tokens"], 12);
        }
        let saved = repo.open(&session.metadata().id).unwrap();
        let serialized = serde_json::to_string(saved.entries()).unwrap();
        assert!(
            !serde_json::to_string(
                &progress
                    .messages
                    .iter()
                    .filter(|m| m.role == MessageRole::Tool)
                    .collect::<Vec<_>>()
            )
            .unwrap()
            .contains(PNG)
        );
        assert!(!serialized.contains("fixture-painter"));
        let records = saved
            .entries()
            .iter()
            .filter_map(|e| match &e.data {
                SessionEntryData::Extension { namespace, data }
                    if namespace == AUXILIARY_NAMESPACE =>
                {
                    Some(data)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1]["attempt"]["usage"]["total_tokens"], 12);
        assert_eq!(provider.calls.lock().unwrap().len(), 2);
    }
}
#[tokio::test]
async fn completed_generation_storage_failure_is_charged_without_retry_or_complete_delivery() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = request(&listener).await;
        respond(socket, response(&format!("data:image/png;base64,{PNG}"))).await;
    });
    let runtime = runtime(root.path(), state.path(), address, false);
    let provider = model(None);
    let store = Arc::new(
        MemoryArtifactStore::new(ArtifactLimits {
            max_artifacts: 1,
            ..Default::default()
        })
        .unwrap(),
    );
    let agent = Agent::new(
        runtime
            .executor_builder(provider.clone())
            .unwrap()
            .artifact_store(store.clone())
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let turn = agent.run("paint", Arc::new(())).await.unwrap();
    server.await.unwrap();
    assert_eq!(turn.execution_result.usage.total_tokens, Some(26));
    let task = &turn.execution_result.auxiliary[0];
    assert_eq!(task.status, "completed");
    assert!(!task.delivery_complete);
    let result = turn
        .execution_result
        .messages
        .iter()
        .find_map(Message::tool_result)
        .unwrap();
    assert!(result.2);
    assert_eq!(result.1["error"]["details"]["operation_completed"], true);
    assert_eq!(result.1["error"]["details"]["delivery_complete"], false);
    assert_eq!(
        result.1["error"]["details"]["captured_artifacts"],
        json!([])
    );
    // Failed staging released its slot; it did not publish a partial image.
    let mut writer = store.begin("image/png").await.unwrap();
    writer.write(&[1]).await.unwrap();
    writer.finish(true).await.unwrap();
    assert_eq!(provider.calls.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn missing_storage_denial_and_zero_task_budget_never_invoke_image_service() {
    for mode in ["no_storage", "denied", "zero_budget"] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let runtime = runtime_with_storage(
            root.path(),
            state.path(),
            listener.local_addr().unwrap(),
            false,
            mode != "no_storage",
        );
        let provider = model(None);
        let mut builder = runtime.executor_builder(provider).unwrap();
        if mode != "no_storage" {
            builder = builder.artifact_store(Arc::new(MemoryArtifactStore::default()));
        }
        builder = if mode == "denied" {
            builder.approval(DenyAllApproval)
        } else {
            builder.approval(AllowAllApproval)
        };
        if mode == "zero_budget" {
            builder = builder.config(TurnExecutionConfig {
                budget: RunBudget {
                    max_auxiliary_calls: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let agent = Agent::new(builder.build().unwrap(), AgentConfig::new("fixture"));
        let outcome = agent
            .run_outcome("paint", Arc::new(()), TurnExecutionContext::new())
            .await;
        assert!(
            outcome.progress().auxiliary.is_empty(),
            "{mode}: {:?}",
            outcome.progress().auxiliary
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        if mode == "zero_budget" {
            assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
        }
    }
}
#[cfg(feature = "code-mode")]
#[tokio::test]
async fn code_mode_generation_explicit_selection_and_reference_editing_keep_canonical_bytes_out() {
    for selected in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for i in 0..2 {
                let (socket, body) = request(&listener).await;
                if i == 1 {
                    assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
                }
                respond(socket, response(&format!("data:image/png;base64,{PNG}"))).await;
            }
        });
        let runtime = runtime(root.path(), state.path(), address, true);
        // Inline image blocks need no decoder. Scoped reference input below requires images.
        let reference = if cfg!(feature = "images") {
            "r.output[1]".into()
        } else {
            format!("{{type:'image',data:'{PNG}',mimeType:'image/png'}}")
        };
        let code = format!(
            "const m=await models.getModelOfType('image','painter','native');const r=await models.generateImages(m,{{input:[{{type:'text',text:'Paint'}}]}});if(r.stopReason!=='stop')throw Error(r.errorMessage);const edit=await models.generateImages(m,{{input:[{{type:'text',text:'Edit'}},{reference}]}});if(edit.stopReason!=='stop')throw Error(edit.errorMessage);{}text('done');",
            if selected {
                "await image(edit.output[1]);"
            } else {
                ""
            }
        );
        let provider = model(Some(code));
        let store = Arc::new(MemoryArtifactStore::default());
        let builder = runtime
            .executor_builder(provider.clone())
            .unwrap()
            .artifact_store(store.clone());
        #[cfg(feature = "images")]
        let builder = builder.tool_image_resolver(Arc::new(
            aicoder_runtime::images::ArtifactImageResolver::new(store),
        ));
        let agent = Agent::new(
            builder.approval(AllowAllApproval).build().unwrap(),
            AgentConfig::new("fixture"),
        );
        let turn = agent.run("paint", Arc::new(())).await.unwrap();
        server.await.unwrap();
        let root = turn
            .execution_result
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .unwrap();
        let (_, value, error) = root.tool_result().unwrap();
        assert!(!error, "{value}");
        assert_eq!(turn.execution_result.auxiliary.len(), 2);
        assert_eq!(turn.execution_result.usage.total_tokens, Some(38));
        assert!(!serde_json::to_string(root).unwrap().contains(PNG));
        let images = root
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::Image { .. }))
            .count();
        assert_eq!(images, usize::from(selected));
        let calls = provider.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .all(|r| r.tools.len() == 1 && r.tools[0].name == "codemode")
        );
        if selected && cfg!(feature = "images") {
            assert!(
                calls[1]
                    .messages
                    .iter()
                    .any(|m| m.content.iter().any(|b| matches!(
                        b,
                        ContentBlock::Image {
                            source: ImageSource::Base64 { .. }
                        }
                    )))
            );
        }
    }
}
#[tokio::test]
async fn cancelled_generation_keeps_an_uncertain_attempt_and_unknown_usage() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let notify = started.clone();
    let server = tokio::spawn(async move {
        let (socket, _) = request(&listener).await;
        notify.notify_one();
        let _socket = socket;
        std::future::pending::<()>().await;
    });
    let runtime = runtime(root.path(), state.path(), address, false);
    let provider = model(None);
    let agent = Agent::new(
        runtime
            .executor_builder(provider)
            .unwrap()
            .artifact_store(Arc::new(MemoryArtifactStore::default()))
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let context = TurnExecutionContext::new();
    let worker = context.clone();
    let task = tokio::spawn(async move { agent.run_outcome("paint", Arc::new(()), worker).await });
    started.notified().await;
    context.cancel("cancel image generation");
    let outcome = task.await.unwrap();
    let task = &outcome.progress().auxiliary[0];
    assert_eq!(task.status, "interrupted");
    assert!(task.tokens.estimated);
    assert!(!task.reported_usage);
    assert!(!task.delivery_complete);
    assert!(outcome.progress().usage.total_tokens.is_none());
    server.abort();
    let _ = server.await;
}
