use aicoder_core::{
    ModelProvider, TurnExecutionContext, artifacts::*, events::*, model::*, tools::*,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
fn limits() -> ArtifactLimits {
    ArtifactLimits {
        max_artifact_bytes: 8192,
        max_total_bytes: 16384,
        max_artifacts: 2,
    }
}
#[tokio::test]
async fn memory_pages_preserve_binary_bytes_and_references_are_immutable() {
    let store = MemoryArtifactStore::new(limits()).unwrap();
    let bytes = (0..4097).map(|i| (i % 256) as u8).collect::<Vec<_>>();
    let reference = store_bytes(&store, "application/octet-stream", &bytes, true)
        .await
        .unwrap();
    let mut actual = Vec::new();
    let mut offset = 0;
    loop {
        let page = store.read(&reference.id, offset, 137).await.unwrap();
        assert_eq!(page.artifact, reference);
        actual.extend(page.bytes);
        if let Some(next) = page.next_offset {
            assert!(next > offset);
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(actual, bytes);
    for (offset, limit) in [(4098, 1), (0, 0), (0, 65537)] {
        assert_eq!(
            store
                .read(&reference.id, offset, limit)
                .await
                .unwrap_err()
                .code,
            "invalid_page"
        );
    }
    assert!(store.read("../../etc/passwd", 0, 1).await.is_err());
    assert!(
        MemoryArtifactStore::default()
            .read(&reference.id, 0, 1)
            .await
            .is_err()
    );
    store.remove(&reference.id).await.unwrap();
    assert_eq!(
        store.read(&reference.id, 0, 1).await.unwrap_err().code,
        "not_found"
    );
}
#[tokio::test]
async fn pending_writers_reserve_quota_and_drop_releases_it() {
    let store = MemoryArtifactStore::new(ArtifactLimits {
        max_artifact_bytes: 100,
        max_total_bytes: 100,
        max_artifacts: 1,
    })
    .unwrap();
    let mut pending = store.begin("text/plain").await.unwrap();
    pending.write(b"unfinished").await.unwrap();
    assert!(store.begin("text/plain").await.is_err());
    drop(pending);
    let mut next = store.begin("text/plain").await.unwrap();
    assert!(next.write(&[0; 101]).await.is_err());
    next.write(b"partial").await.unwrap();
    let reference = next.finish(false).await.unwrap();
    assert!(!reference.complete);
    assert_eq!(reference.bytes, 7);
    assert!(store.begin("text/plain").await.is_err());
    store.remove(&reference.id).await.unwrap();
    assert!(store.begin("text/plain").await.is_ok());
}
struct Output {
    calls: Arc<AtomicUsize>,
    fail: bool,
}
fn original() -> Value {
    json!({"key":"value","payload":"编译\n\"结果\"".repeat(180)})
}
#[async_trait]
impl ExecutableTool for Output {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "output".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err(ToolFailure::new("test_failure", "operation failed").with_details(original()))
        } else {
            Ok(ToolSuccess::new(original()))
        }
    }
}
async fn dispatch(
    store: Arc<dyn ArtifactStore>,
    fail: bool,
) -> (Message, Vec<AgentRawEventEnvelope>, usize) {
    dispatch_with_budget(store, fail, 512).await
}
async fn dispatch_with_budget(
    store: Arc<dyn ArtifactStore>,
    fail: bool,
    display: usize,
) -> (Message, Vec<AgentRawEventEnvelope>, usize) {
    let root = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::default();
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .register(Output {
            calls: calls.clone(),
            fail,
        })
        .unwrap();
    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        root.path(),
        Arc::new(AllowAllApproval),
        DispatcherConfig {
            max_output_bytes: display,
            ..Default::default()
        },
    )
    .unwrap()
    .with_artifact_store(store)
    .unwrap();
    let frames = Arc::new(Mutex::new(Vec::new()));
    let output = frames.clone();
    let events = AgentEventEmitter::new(Arc::new(move |frame: &AgentRawEventEnvelope| {
        output.lock().unwrap().push(frame.clone())
    }));
    let message = dispatcher
        .dispatch_with_events(
            &[ModelToolCall {
                id: "call".into(),
                name: "output".into(),
                arguments: json!({}),
            }],
            &events,
        )
        .await
        .unwrap()
        .remove(0);
    events.shutdown().await;
    let frames = frames.lock().unwrap().clone();
    (message, frames, calls.load(Ordering::SeqCst))
}
#[tokio::test]
async fn truncated_tool_outputs_keep_recoverable_originals_and_event_references() {
    for fail in [false, true] {
        let store = Arc::new(MemoryArtifactStore::new(limits()).unwrap());
        let (message, frames, calls) = dispatch(store.clone(), fail).await;
        assert_eq!(calls, 1);
        let (_, content, is_error) = message.tool_result().unwrap();
        assert_eq!(is_error, fail);
        let reference: ArtifactRef =
            serde_json::from_value(content["artifacts"][0].clone()).unwrap();
        let page = store.read(&reference.id, 0, 8192).await.unwrap();
        assert!(page.artifact.complete);
        assert_eq!(
            serde_json::from_slice::<Value>(&page.bytes).unwrap(),
            original()
        );
        let payload = if fail {
            &content["error"]["details"]["preview"]
        } else {
            &content["output"]
        };
        assert!(payload.to_string().len() <= 512);
        assert!(!payload.to_string().contains('\u{fffd}'));
        assert!(frames.iter().any(|frame|matches!(&frame.event,AgentRawEvent::ToolExecutionEnded {artifacts,..} if artifacts==std::slice::from_ref(&reference))));
    }
}
#[tokio::test]
async fn unavailable_or_oversized_archives_never_repeat_or_change_tool_execution() {
    let store = Arc::new(
        MemoryArtifactStore::new(ArtifactLimits {
            max_artifact_bytes: 8192,
            max_total_bytes: 8192,
            max_artifacts: 1,
        })
        .unwrap(),
    );
    let reserved = store.begin("text/plain").await.unwrap();
    let (message, _, calls) = dispatch(store, false).await;
    let result = message.tool_result().unwrap().1;
    assert_eq!(calls, 1);
    assert_eq!(result["ok"], true);
    assert_eq!(result["truncated"], true);
    assert_eq!(result["artifact_error"], "quota_exceeded");
    assert!(result.get("artifacts").is_none());
    drop(reserved);
    let store = Arc::new(
        MemoryArtifactStore::new(ArtifactLimits {
            max_artifact_bytes: 600,
            max_total_bytes: 600,
            max_artifacts: 1,
        })
        .unwrap(),
    );
    let (message, _, calls) = dispatch(store, false).await;
    assert_eq!(calls, 1);
    assert_eq!(
        message.tool_result().unwrap().1["artifact_error"],
        "too_large"
    );
    let store = Arc::new(
        MemoryArtifactStore::new(ArtifactLimits {
            max_artifact_bytes: 100,
            max_total_bytes: 100,
            max_artifacts: 1,
        })
        .unwrap(),
    );
    let (message, _, calls) = dispatch_with_budget(store, false, 8192).await;
    assert_eq!(calls, 1);
    let result = message.tool_result().unwrap().1;
    assert_eq!(result["output"], original());
    assert_eq!(result["truncated"], false);
    assert!(result.get("artifact_error").is_none());
}
#[tokio::test]
async fn artifact_model_pages_fit_serialized_budget_and_advance_raw_offsets() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryArtifactStore::new(limits()).unwrap());
    let bytes = [0, 255, 10, 1, 128, 65].repeat(300);
    let reference = store_bytes(store.as_ref(), "application/octet-stream", &bytes, false)
        .await
        .unwrap();
    let context = ToolContext::new(root.path(), std::time::Duration::from_secs(1), 1024)
        .unwrap()
        .with_artifact_store(store);
    let mut offset = 0;
    loop {
        let page = ReadArtifactTool
            .execute(
                &context,
                json!({"id":reference.id,"offset":offset,"max_bytes":65536}),
            )
            .await
            .unwrap()
            .output;
        assert!(page.to_string().len() <= 1024);
        assert_eq!(page["artifact"]["complete"], false);
        assert_eq!(page["encoding"], "utf8_lossy");
        let read = page["bytes_read"].as_u64().unwrap();
        assert_eq!(
            page["content"],
            String::from_utf8_lossy(&bytes[offset as usize..(offset + read) as usize]).as_ref()
        );
        if let Some(next) = page["next_offset"].as_u64() {
            assert_eq!(next, offset + read);
            assert!(next > offset);
            offset = next;
        } else {
            assert_eq!(offset + read, bytes.len() as u64);
            break;
        }
    }
}

struct Stalled {
    dropped: Arc<AtomicUsize>,
}
struct StalledWriter(Arc<AtomicUsize>);
impl Drop for StalledWriter {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl ArtifactWriter for StalledWriter {
    async fn write(&mut self, _: &[u8]) -> Result<(), ArtifactError> {
        std::future::pending().await
    }
    async fn finish(self: Box<Self>, _: bool) -> Result<ArtifactRef, ArtifactError> {
        std::future::pending().await
    }
}
#[async_trait]
impl ArtifactStore for Stalled {
    fn limits(&self) -> ArtifactLimits {
        limits()
    }
    async fn begin(&self, _: &str) -> Result<Box<dyn ArtifactWriter>, ArtifactError> {
        Ok(Box::new(StalledWriter(self.dropped.clone())))
    }
    async fn read(&self, _: &str, _: u64, _: usize) -> Result<ArtifactPage, ArtifactError> {
        Err(ArtifactError::new("not_found"))
    }
    async fn remove(&self, _: &str) -> Result<(), ArtifactError> {
        Ok(())
    }
}
#[tokio::test(start_paused = true)]
async fn stalled_storage_expires_without_repeating_tool_or_leaking_staged_writer() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let (message, _, calls) = dispatch(
        Arc::new(Stalled {
            dropped: dropped.clone(),
        }),
        false,
    )
    .await;
    assert_eq!(calls, 1);
    assert_eq!(
        message.tool_result().unwrap().1["artifact_error"],
        "storage_timeout"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

struct OneCall(Arc<AtomicUsize>);
#[async_trait]
impl ModelProvider for OneCall {
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
        self.0.fetch_add(1, Ordering::SeqCst);
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "call".into(),
                        name: "output".into(),
                        arguments: json!({}),
                    },
                }],
            },
            stop_reason: StopReason::ToolCalls,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::test]
async fn output_budget_stop_retains_artifact_reference_and_complete_tool_pair_in_history() {
    use aicoder_core::session::{MemorySessionRepository, SessionRepository};
    use aicoder_core::{
        Agent, AgentConfig, AgentTurnOutcome, RunBudget, SessionSelection, TurnExecutionConfig,
        TurnExecutor,
    };
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryArtifactStore::new(limits()).unwrap());
    let model_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry
        .register(Output {
            calls: tool_calls.clone(),
            fail: false,
        })
        .unwrap();
    let executor = TurnExecutor::builder(OneCall(model_calls.clone()))
        .workspace(root.path())
        .registry(registry)
        .artifact_store(store.clone())
        .dispatcher_config(DispatcherConfig {
            max_output_bytes: 512,
            ..Default::default()
        })
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_output_bytes: Some(20),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let agent = Agent::new(executor, AgentConfig::new("offline"));
    let repo = MemorySessionRepository::new();
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::New,
            "run tool",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Limited(_)),
        "{outcome:?}"
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    assert!(outcome.progress().output_bytes <= 20);
    let session = repo.open(&outcome.session().unwrap().id).unwrap();
    let context = session
        .selected_branch()
        .model_context()
        .unwrap()
        .into_messages();
    let result = context.iter().find_map(|m| m.tool_result()).unwrap();
    assert_eq!(result.0, "call");
    assert_eq!(result.1["truncated"], true);
    let reference: ArtifactRef = serde_json::from_value(result.1["artifacts"][0].clone()).unwrap();
    let page = store.read(&reference.id, 0, 8192).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&page.bytes).unwrap(),
        original()
    );
    assert!(
        context
            .iter()
            .any(|m| m.tool_calls().any(|call| call.id == "call"))
    );
}

struct PreparationFailure;
#[async_trait]
impl ExecutableTool for PreparationFailure {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "prepare_failure".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn prepare(
        &self,
        _: &ToolContext,
        _: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        Err(ToolFailure::new("prepare_failed", "preparation failed").with_details(original()))
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        panic!("failed preparation must never execute")
    }
}
#[tokio::test]
async fn preparation_failure_uses_the_same_bounded_archive_pipeline() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryArtifactStore::new(limits()).unwrap());
    let mut registry = ToolRegistry::default();
    registry.register(PreparationFailure).unwrap();
    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        root.path(),
        Arc::new(AllowAllApproval),
        DispatcherConfig {
            max_output_bytes: 512,
            ..Default::default()
        },
    )
    .unwrap()
    .with_artifact_store(store.clone())
    .unwrap();
    let events = AgentEventEmitter::new(Arc::new(()));
    let messages = dispatcher
        .dispatch_with_events(
            &[ModelToolCall {
                id: "prepare".into(),
                name: "prepare_failure".into(),
                arguments: json!({}),
            }],
            &events,
        )
        .await
        .unwrap();
    let (_, value, is_error) = messages[0].tool_result().unwrap();
    assert!(is_error);
    assert_eq!(value["error"]["code"], "prepare_failed");
    assert!(value["error"]["details"].to_string().len() <= 512);
    let reference: ArtifactRef = serde_json::from_value(value["artifacts"][0].clone()).unwrap();
    let page = store.read(&reference.id, 0, 8192).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&page.bytes).unwrap(),
        original()
    );
}

struct ReferencedOutput(Vec<ArtifactRef>);
#[async_trait]
impl ExecutableTool for ReferencedOutput {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "referenced_output".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        Ok(ToolSuccess {
            output: original(),
            truncated: false,
            artifacts: self.0.clone(),
        })
    }
}
#[tokio::test]
async fn full_reference_slots_keep_existing_sources_and_skip_unreferenced_auto_archives() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(
        MemoryArtifactStore::new(ArtifactLimits {
            max_artifact_bytes: 8192,
            max_total_bytes: 131072,
            max_artifacts: 9,
        })
        .unwrap(),
    );
    let mut references = Vec::new();
    for _ in 0..8 {
        references.push(
            store_bytes(store.as_ref(), "text/plain", b"original source", true)
                .await
                .unwrap(),
        );
    }
    let mut registry = ToolRegistry::default();
    registry
        .register(ReferencedOutput(references.clone()))
        .unwrap();
    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        root.path(),
        Arc::new(AllowAllApproval),
        DispatcherConfig {
            max_output_bytes: 512,
            ..Default::default()
        },
    )
    .unwrap()
    .with_artifact_store(store.clone())
    .unwrap();
    let events = AgentEventEmitter::new(Arc::new(()));
    let messages = dispatcher
        .dispatch_with_events(
            &[ModelToolCall {
                id: "refs".into(),
                name: "referenced_output".into(),
                arguments: json!({}),
            }],
            &events,
        )
        .await
        .unwrap();
    let result = messages[0].tool_result().unwrap().1;
    assert_eq!(result["artifact_error"], "reference_limit");
    assert_eq!(result["artifacts"], json!(references));
    assert_eq!(result["truncated"], true);
    // A ninth slot remains available: the dispatcher did not publish an unreachable file.
    assert!(store.begin("text/plain").await.is_ok());
}
