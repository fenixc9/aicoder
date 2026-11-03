//! Native acquisition stays codec-free; optional projection validates pixels later.
use aicoder_core::{artifacts::*, model::*, tools::*, *};
use aicoder_runtime::{tools::ReadFileTool, *};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

fn bmp() -> Vec<u8> {
    let mut bytes = vec![0; 62];
    bytes[..2].copy_from_slice(b"BM");
    for (at, value) in [(2, 62u32), (10, 54), (14, 40), (18, 2), (22, 1), (34, 8)] {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
    bytes[54..60].copy_from_slice(&[0, 0, 255, 0, 255, 0]);
    bytes
}
fn context(root: &Path, store: Arc<dyn ArtifactStore>, limit: usize) -> ToolContext {
    ToolContext::new(root, Duration::from_secs(3), limit)
        .unwrap()
        .with_artifact_store(store)
}
#[tokio::test]
async fn binary_capture_uses_signature_preserves_raw_bytes_and_needs_no_codec() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryArtifactStore::default());
    let original = bmp();
    fs::write(root.path().join("picture.txt"), &original).unwrap();
    let result = ReadFileTool
        .execute(
            &context(root.path(), store.clone(), 4096),
            json!({"path":"picture.txt"}),
        )
        .await
        .unwrap();
    assert_eq!(result.output["kind"], "image");
    assert_eq!(result.output["media_type"], "image/bmp");
    assert_eq!(result.output["source_bytes"], original.len());
    assert_eq!(result.output["delivery_complete"], true);
    assert_eq!(result.output["has_more"], false);
    assert!(result.output.get("next_cursor").is_none());
    assert_eq!(result.artifacts.len(), 1);
    assert!(result.artifacts[0].complete);
    fs::write(root.path().join("picture.txt"), "replaced after capture").unwrap();
    assert_eq!(
        store
            .read(&result.artifacts[0].id, 0, 4096)
            .await
            .unwrap()
            .bytes,
        original
    );
    fs::write(root.path().join("ordinary.bmp"), "BM is ordinary text\n").unwrap();
    let text = ReadFileTool
        .execute(
            &context(root.path(), store, 4096),
            json!({"path":"ordinary.bmp"}),
        )
        .await
        .unwrap();
    assert_eq!(text.output["content"], "BM is ordinary text");
    assert!(text.artifacts.is_empty());
}
#[tokio::test]
async fn pagination_stale_cursors_paths_source_and_metadata_budgets_remain_explicit() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    fs::write(root.path().join("picture"), bmp()).unwrap();
    let store = Arc::new(MemoryArtifactStore::default());
    let ctx = context(root.path(), store.clone(), 512);
    for args in [
        json!({"path":"picture","offset_line":2}),
        json!({"path":"picture","limit_lines":1}),
    ] {
        assert_eq!(
            ReadFileTool.execute(&ctx, args).await.unwrap_err().code,
            "image_pagination"
        );
    }
    assert!(
        ReadFileTool
            .execute(&ctx, json!({"path":"picture","offset_line":1}))
            .await
            .is_ok()
    );
    let without = ToolContext::new(root.path(), Duration::from_secs(3), 4096).unwrap();
    assert_eq!(
        ReadFileTool
            .execute(&without, json!({"path":"picture"}))
            .await
            .unwrap_err()
            .code,
        "artifact_store_unavailable"
    );
    let tiny = Arc::new(
        MemoryArtifactStore::new(ArtifactLimits {
            max_artifact_bytes: 32,
            max_total_bytes: 64,
            max_artifacts: 2,
        })
        .unwrap(),
    );
    assert_eq!(
        ReadFileTool
            .execute(&context(root.path(), tiny, 512), json!({"path":"picture"}))
            .await
            .unwrap_err()
            .code,
        "image_source_limit"
    );
    let huge = fs::File::create(root.path().join("large")).unwrap();
    std::io::Write::write_all(&mut &huge, &bmp()).unwrap();
    huge.set_len(8 * 1024 * 1024 + 1).unwrap();
    assert_eq!(
        ReadFileTool
            .execute(&ctx, json!({"path":"large"}))
            .await
            .unwrap_err()
            .code,
        "image_source_limit"
    );
    fs::write(root.path().join("text"), "line\n".repeat(100)).unwrap();
    let page = ReadFileTool
        .execute(&ctx, json!({"path":"text","limit_lines":1}))
        .await
        .unwrap();
    fs::write(root.path().join("text"), bmp()).unwrap();
    assert_eq!(
        ReadFileTool
            .execute(
                &ctx,
                json!({"path":"text","cursor":page.output["next_cursor"]})
            )
            .await
            .unwrap_err()
            .code,
        "stale_cursor"
    );
    fs::write(other.path().join("outside"), bmp()).unwrap();
    assert_eq!(
        ReadFileTool
            .execute(&ctx, json!({"path":other.path().join("outside")}))
            .await
            .unwrap_err()
            .code,
        "path_outside_workspace"
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(other.path().join("outside"), root.path().join("escape"))
            .unwrap();
        assert_eq!(
            ReadFileTool
                .execute(&ctx, json!({"path":"escape"}))
                .await
                .unwrap_err()
                .code,
            "path_outside_workspace"
        );
    }
    let empty = Arc::new(
        MemoryArtifactStore::new(ArtifactLimits {
            max_artifact_bytes: 8192,
            max_total_bytes: 8192,
            max_artifacts: 1,
        })
        .unwrap(),
    );
    let mut path = root.path().to_path_buf();
    for _ in 0..5 {
        path.push("long".repeat(30));
        fs::create_dir(&path).unwrap();
    }
    path.push("picture");
    fs::write(&path, bmp()).unwrap();
    assert_eq!(
        ReadFileTool
            .execute(
                &context(root.path(), empty.clone(), 512),
                json!({"path":path})
            )
            .await
            .unwrap_err()
            .code,
        "output_limit_too_small"
    );
    assert!(empty.begin("image/bmp").await.is_ok());
}
struct PendingStore {
    inner: MemoryArtifactStore,
    started: tokio::sync::Notify,
}
#[async_trait]
impl ArtifactStore for PendingStore {
    fn limits(&self) -> ArtifactLimits {
        self.inner.limits()
    }
    async fn begin(&self, mime: &str) -> Result<Box<dyn ArtifactWriter>, ArtifactError> {
        let writer = self.inner.begin(mime).await?;
        self.started.notify_one();
        std::future::pending::<()>().await;
        Ok(writer)
    }
    async fn read(
        &self,
        id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<ArtifactPage, ArtifactError> {
        self.inner.read(id, offset, limit).await
    }
    async fn remove(&self, id: &str) -> Result<(), ArtifactError> {
        self.inner.remove(id).await
    }
}
#[tokio::test]
async fn archive_failure_timeout_and_dropped_capture_do_not_repeat_reads_or_retain_reservations() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("picture"), bmp()).unwrap();
    let limits = ArtifactLimits {
        max_artifact_bytes: 8192,
        max_total_bytes: 8192,
        max_artifacts: 1,
    };
    let full = Arc::new(MemoryArtifactStore::new(limits).unwrap());
    let existing = store_bytes(full.as_ref(), "text/plain", b"occupied", true)
        .await
        .unwrap();
    let error = ReadFileTool
        .execute(
            &context(root.path(), full.clone(), 4096),
            json!({"path":"picture"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "image_capture_failed");
    assert_eq!(error.details.unwrap()["operation_completed"], true);
    assert_eq!(
        full.read(&existing.id, 0, 100).await.unwrap().bytes,
        b"occupied"
    );
    fs::write(root.path().join("picture.txt"), bmp()).unwrap();
    let provider = Arc::new(Provider(true, Mutex::new(vec![])));
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .registry(aicoder_runtime::tools::default_registry().unwrap())
            .artifact_store(full)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let turn = agent.run("inspect", Arc::new(())).await.unwrap();
    let results = turn
        .execution_result
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1);
    let (_, result, is_error) = results[0].tool_result().unwrap();
    assert!(is_error);
    assert_eq!(result["error"]["code"], "image_capture_failed");
    assert_eq!(result["error"]["details"]["operation_completed"], true);
    assert_eq!(provider.1.lock().unwrap().len(), 2);
    for abort in [false, true] {
        let store = Arc::new(PendingStore {
            inner: MemoryArtifactStore::new(limits).unwrap(),
            started: tokio::sync::Notify::new(),
        });
        let ctx = context(root.path(), store.clone(), 4096);
        let task =
            tokio::spawn(
                async move { ReadFileTool.execute(&ctx, json!({"path":"picture"})).await },
            );
        store.started.notified().await;
        if abort {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            let error = task.await.unwrap().unwrap_err();
            assert_eq!(error.details.unwrap()["capture_error"], "storage_timeout");
        }
        assert!(store.inner.begin("image/bmp").await.is_ok());
    }
}
struct Provider(bool, Mutex<Vec<ModelRequest>>);
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.tools = true;
        spec.capabilities.images = self.0;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        _: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        request.validate(&self.model_spec(&request.model)?)?;
        let done = request.messages.iter().any(|m| m.role == MessageRole::Tool);
        self.1.lock().unwrap().push(request.clone());
        let message = if done {
            Message::text(MessageRole::Assistant, "done")
        } else {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "image".into(),
                        name: "read_file".into(),
                        arguments: json!({"path":"picture.txt"}),
                    },
                }],
            }
        };
        Ok(ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if done {
                StopReason::Complete
            } else {
                StopReason::ToolCalls
            },
            usage: ModelUsage::zero(),
        })
    }
}
#[tokio::test]
async fn runtime_native_images_reach_declared_models_and_keep_raw_canonical_facts() {
    for vision in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(root.path().join("picture.txt"), bmp()).unwrap();
        let runtime = Runtime::load_with_environment(
            root.path(),
            RuntimeOverrides {
                settings: Settings {
                    resources: aicoder_runtime::resources::ResourceSettings {
                        enabled: Setting::Value(false),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                session_dir: Setting::Value(state.path().into()),
                ..Default::default()
            },
            ConfigDiscovery {
                enabled: false,
                ..Default::default()
            },
            Arc::new(BTreeMap::new()),
        )
        .unwrap();
        let provider = Arc::new(Provider(vision, Mutex::new(vec![])));
        let agent = Agent::new(
            runtime
                .executor_builder(provider.clone())
                .unwrap()
                .build()
                .unwrap(),
            AgentConfig::new("fixture"),
        );
        let turn = agent.run("inspect", Arc::new(())).await.unwrap();
        let canonical = turn
            .execution_result
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .unwrap();
        assert!(matches!(
            &canonical.content[1],
            ContentBlock::Image {
                source: ImageSource::Artifact { .. }
            }
        ));
        assert!(
            canonical
                .tool_result()
                .unwrap()
                .1
                .get("_tool_image_projection")
                .is_none()
        );
        let requests = provider.1.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let view = requests[1]
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .unwrap();
        let status = view.tool_result().unwrap().1["_tool_image_projection"][0]["status"]
            .as_str()
            .unwrap();
        if vision && cfg!(feature = "images") {
            assert_eq!(status, "normalized_preview");
            assert!(matches!(
                &view.content[1],
                ContentBlock::Image {
                    source: ImageSource::Base64 { .. }
                }
            ));
        } else {
            assert_eq!(view.content.len(), 1);
            assert_eq!(
                status,
                if vision {
                    "unavailable"
                } else {
                    "model_without_vision"
                }
            );
        }
    }
}

#[cfg(feature = "images")]
#[tokio::test]
async fn all_five_native_formats_archive_originals_and_produce_bounded_static_previews() {
    use aicoder_core::tool_images::ToolImageResolver;
    use aicoder_runtime::images::ArtifactImageResolver;
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryArtifactStore::default());
    let ctx = context(root.path(), store.clone(), 4096);
    for (format, mime) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::Gif, "image/gif"),
        (ImageFormat::WebP, "image/webp"),
        (ImageFormat::Bmp, "image/bmp"),
    ] {
        let mut bytes = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(30, 20, Rgb([200, 50, 20])))
            .write_to(&mut bytes, format)
            .unwrap();
        let original = bytes.into_inner();
        fs::write(root.path().join("wrong.txt"), &original).unwrap();
        let result = ReadFileTool
            .execute(&ctx, json!({"path":"wrong.txt"}))
            .await
            .unwrap();
        assert_eq!(result.output["media_type"], mime);
        assert_eq!(
            store
                .read(&result.artifacts[0].id, 0, 65536)
                .await
                .unwrap()
                .bytes,
            original
        );
        let preview = ArtifactImageResolver::new(store.clone())
            .resolve(&result.artifacts[0], TurnExecutionContext::new())
            .await
            .unwrap();
        let ImageSource::Base64 { media_type, data } = preview.source() else {
            panic!("not inline")
        };
        assert_eq!(media_type, "image/jpeg");
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert!(bytes.len() <= 256 * 1024);
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 30);
    }
}
