#![cfg(feature = "code-mode")]
use aicoder_codemode::{CodeModeConfig, CodeModeTool};
use aicoder_core::{artifacts::*, composition::*, model::*, session::*, tools::*, *};
use aicoder_runtime::{
    code_mode::CodeModeSettings, resources::ResourceSettings, tools::ImageOutputTool, *,
};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAIAAAB7QOjdAAAAD0lEQVR4nGP4z8DA8J8BAAf/Af8Bf4mnAAAAAElFTkSuQmCC";
struct Model {
    code: String,
    requests: Mutex<Vec<ModelRequest>>,
    vision: bool,
}
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.tools = true;
        spec.capabilities.images = self.vision;
        spec.context_tokens = Some(65536);
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
        self.requests.lock().unwrap().push(request.clone());
        Ok(ModelResponse {
            id: None,
            model: request.model,
            message: if done {
                Message::text(MessageRole::Assistant, "done")
            } else {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "program".into(),
                            name: "codemode".into(),
                            arguments: json!({"code":self.code}),
                        },
                    }],
                }
            },
            stop_reason: if done {
                StopReason::Complete
            } else {
                StopReason::ToolCalls
            },
            usage: ModelUsage::zero(),
        })
    }
}
fn runtime(root: &Path, state: &Path, tools: Option<Vec<String>>) -> Runtime {
    Runtime::load_with_environment(
        root,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                code_mode: CodeModeSettings {
                    enabled: Setting::Value(true),
                    only: Setting::Value(true),
                    tools: tools.map(Setting::Value).unwrap_or(Setting::Inherit),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Value(state.into()),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::new()),
    )
    .unwrap()
}
fn root_result(messages: &[Message]) -> &Message {
    messages
        .iter()
        .find(|m| m.tool_result().is_some_and(|(id, _, _)| id == "program"))
        .unwrap()
}
fn count_images(messages: &[Message]) -> usize {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|b| matches!(b, ContentBlock::Image { .. }))
        .count()
}
#[tokio::test]
async fn filtering_leaf_images_preserves_canonical_facts_without_model_exposure() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    use base64::Engine;
    fs::write(
        root.path().join("picture"),
        base64::engine::general_purpose::STANDARD
            .decode(PNG)
            .unwrap(),
    )
    .unwrap();
    let runtime = runtime(root.path(), state.path(), Some(vec!["read_file".into()]));
    let provider = Arc::new(Model {
        code: "const data=await tools.read_file({path:'picture'}); text(data.kind);".into(),
        requests: Mutex::new(vec![]),
        vision: true,
    });
    let agent = Agent::new(
        runtime
            .executor_builder(provider.clone())
            .unwrap()
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let repo = runtime.sessions().unwrap();
    let session = repo.create(root.path()).unwrap();
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "inspect",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    let saved = repo.open(&session.metadata().id).unwrap();
    let leaves = saved
        .entries()
        .iter()
        .filter_map(|e| match &e.data {
            SessionEntryData::Extension { namespace, data }
                if namespace == COMPOSITION_NAMESPACE =>
            {
                serde_json::from_value::<CompositionRecord>(data.clone()).ok()
            }
            _ => None,
        })
        .filter_map(|record| match record {
            CompositionRecord::Result { message, .. } => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(count_images(&leaves), 1);
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(count_images(&requests[1].messages), 0);
    let result = root_result(&requests[1].messages).tool_result().unwrap().1;
    assert_eq!(result["output"]["program"]["items"], json!(["image"]));
    assert!(result.get("artifacts").is_none());
}
#[tokio::test]
async fn explicit_reference_and_inline_outputs_reach_model_views_and_keep_raw_leaf_history() {
    for vision in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        use base64::Engine;
        fs::write(
            root.path().join("picture"),
            base64::engine::general_purpose::STANDARD
                .decode(PNG)
                .unwrap(),
        )
        .unwrap();
        let runtime = runtime(root.path(), state.path(), None);
        let code = format!(
            "const ordinary=await tools.read_file({{path:'picture'}}); if(ordinary.output!==undefined) throw Error('normal output changed'); const full=await tools.read_file.result({{path:'picture'}}); await image(full); await image({{data:'{PNG}',mimeType:' IMAGE/PNG ; profile=fixture'}}); text('selected');"
        );
        let provider = Arc::new(Model {
            code,
            requests: Mutex::new(vec![]),
            vision,
        });
        let agent = Agent::new(
            runtime
                .executor_builder(provider.clone())
                .unwrap()
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
                "inspect",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
        let canonical = root_result(&turn.execution_result.messages);
        assert_eq!(
            count_images(std::slice::from_ref(canonical)),
            2,
            "{canonical:?}"
        );
        let (_, value, error) = canonical.tool_result().unwrap();
        assert!(!error, "{value}");
        assert_eq!(value["output"]["composition"]["calls"], 4);
        assert_eq!(value["output"]["program"]["items"], json!(["selected"]));
        assert_eq!(value["artifacts"].as_array().unwrap().len(), 2);
        let saved = repo.open(&session.metadata().id).unwrap();
        assert!(
            !serde_json::to_string(saved.entries())
                .unwrap()
                .contains("_tool_image_projection")
        );
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| request.tools.len() == 1 && request.tools[0].name == "codemode")
        );
        assert_eq!(
            count_images(&requests[1].messages),
            if vision && cfg!(feature = "images") {
                2
            } else {
                0
            }
        );
        assert_eq!(
            root_result(&requests[1].messages).tool_result().unwrap().1["_tool_image_projection"]
                [0]["status"],
            if !vision {
                "model_without_vision"
            } else if cfg!(feature = "images") {
                "normalized_preview"
            } else {
                "unavailable"
            }
        );
    }
}
#[tokio::test]
async fn image_intents_respect_allowlists_readonly_capabilities_limits_and_parent_budgets() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    for (tools, code, expected) in [
        (
            Some(vec!["read_file".into()]),
            format!("await image({{data:'{PNG}',mimeType:'image/png'}});"),
            "catalog",
        ),
        (
            None,
            format!(
                "for(let i=0;i<5;i++) try {{ await image({{data:'{PNG}',mimeType:'image/png'}}); }} catch(e) {{ text(e.code); throw e; }}"
            ),
            "composition_image_limit",
        ),
    ] {
        let runtime = runtime(root.path(), state.path(), tools);
        let provider = Arc::new(Model {
            code,
            requests: Mutex::new(vec![]),
            vision: false,
        });
        let agent = Agent::new(
            runtime
                .executor_builder(provider)
                .unwrap()
                .approval(AllowAllApproval)
                .build()
                .unwrap(),
            AgentConfig::new("fixture"),
        );
        let turn = agent.run("inspect", Arc::new(())).await.unwrap();
        let (_, value, error) = root_result(&turn.execution_result.messages)
            .tool_result()
            .unwrap();
        assert!(error, "{value}");
        assert!(value.to_string().contains(expected), "{value}");
        assert_eq!(
            count_images(&turn.execution_result.messages),
            if expected == "composition_image_limit" {
                4
            } else {
                0
            }
        );
    }
    let provider = Arc::new(Model {
        code: "try {await __image({path:'forbidden',content:'bad'});} catch(e) {text(e.code);}"
            .into(),
        requests: Mutex::new(vec![]),
        vision: false,
    });
    let mut config = CodeModeConfig::default();
    config.image_tool = Some("write_file".into());
    let mut registry = aicoder_runtime::tools::default_registry().unwrap();
    registry
        .register(CodeModeTool::new(config).unwrap())
        .unwrap();
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider)
            .workspace(root.path())
            .registry(registry)
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let turn = agent.run("inspect", Arc::new(())).await.unwrap();
    assert!(!root.path().join("forbidden").exists());
    assert_eq!(
        root_result(&turn.execution_result.messages)
            .tool_result()
            .unwrap()
            .1["output"]["program"]["items"],
        json!(["image_tool_not_read_only"])
    );
    let runtime = runtime(root.path(), state.path(), None);
    let provider = Arc::new(Model {
        code: format!("await image({{data:'{PNG}',mimeType:'image/png'}});"),
        requests: Mutex::new(vec![]),
        vision: true,
    });
    let agent = Agent::new(
        runtime
            .executor_builder(provider.clone())
            .unwrap()
            .approval(AllowAllApproval)
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_tool_calls: Some(1),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let outcome = agent
        .run_outcome("inspect", Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn image_output_cannot_restore_foreign_storage_authority_and_reports_bad_inputs() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryArtifactStore::default());
    let reference = store_bytes(store.as_ref(), "image/png", b"raw", true)
        .await
        .unwrap();
    let foreign = ToolContext::new(root.path(), Duration::from_secs(3), 4096)
        .unwrap()
        .with_artifact_store(Arc::new(MemoryArtifactStore::default()));
    assert_eq!(
        ImageOutputTool
            .execute(&foreign, json!({"artifact_id":reference.id}))
            .await
            .unwrap_err()
            .code,
        "image_output_unavailable"
    );
    let context = ToolContext::new(root.path(), Duration::from_secs(3), 4096)
        .unwrap()
        .with_artifact_store(store);
    for args in [
        json!({"artifact_id":"../../outside"}),
        json!({"data":"AQID","media_type":"text/plain"}),
        json!({"data":"!bad","media_type":"image/png"}),
        json!({"data":"AQID","media_type":"image/png","artifact_id":reference.id}),
    ] {
        assert!(ImageOutputTool.execute(&context, args).await.is_err());
    }
    assert_eq!(
        ImageOutputTool
            .execute(&context, json!({"artifact_id":reference.id}))
            .await
            .unwrap()
            .artifacts[0],
        reference
    );
}

#[tokio::test]
async fn image_tool_guards_are_not_bypassed_by_the_explicit_output_marker() {
    use aicoder_core::extensions::*;
    struct Guard;
    #[async_trait]
    impl ToolHook for Guard {
        async fn before_tool(
            &self,
            input: ToolHookInput<'_>,
            _: ExtensionCancellation,
        ) -> anyhow::Result<ToolHookDecision> {
            Ok(if input.call.name == "image_output" {
                ToolHookDecision::Deny {
                    reason: "Host image guard".into(),
                }
            } else {
                ToolHookDecision::Continue
            })
        }
    }
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path(), state.path(), None);
    let mut extension = Extension::new("image-guard", "1").unwrap();
    extension
        .tool_hook(HookSpec::new("guard"), Arc::new(Guard))
        .unwrap();
    let mut extensions = ExtensionRegistry::new();
    extensions.install(extension).unwrap();
    let provider = Arc::new(Model {
        code: format!(
            "try {{await image({{data:'{PNG}',mimeType:'image/png'}});}} catch(e) {{text(e.code);}}"
        ),
        requests: Mutex::new(vec![]),
        vision: true,
    });
    let agent = Agent::new(
        runtime
            .executor_builder_with_extensions(provider.clone(), Arc::new(extensions.finish()))
            .unwrap()
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let turn = agent.run("inspect", Arc::new(())).await.unwrap();
    assert_eq!(count_images(&turn.execution_result.messages), 0);
    let output = root_result(&turn.execution_result.messages)
        .tool_result()
        .unwrap()
        .1;
    assert_eq!(
        output["output"]["program"]["items"],
        json!(["extension_denied"])
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
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
async fn cancelled_or_dropped_image_programs_release_writers_and_vm_owners() {
    for drop_future in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(PendingStore {
            inner: MemoryArtifactStore::new(ArtifactLimits {
                max_artifact_bytes: 8192,
                max_total_bytes: 8192,
                max_artifacts: 1,
            })
            .unwrap(),
            started: tokio::sync::Notify::new(),
        });
        let mode = Arc::new(CodeModeTool::new(CodeModeConfig::default()).unwrap());
        let mut registry = ToolRegistry::default();
        registry.register(ImageOutputTool).unwrap();
        registry.register_shared(mode.clone()).unwrap();
        let provider = Arc::new(Model {
            code: format!("await image({{data:'{PNG}',mimeType:'image/png'}});store('late',1);"),
            requests: Mutex::new(vec![]),
            vision: true,
        });
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(provider.clone())
                .workspace(root.path())
                .registry(registry)
                .artifact_store(store.clone())
                .approval(AllowAllApproval)
                .build()
                .unwrap(),
            AgentConfig::new("fixture"),
        );
        let context = TurnExecutionContext::new();
        let worker = context.clone();
        let task =
            tokio::spawn(async move { agent.run_outcome("inspect", Arc::new(()), worker).await });
        store.started.notified().await;
        if drop_future {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            context.cancel("stop image program");
            assert!(!matches!(
                task.await.unwrap(),
                AgentTurnOutcome::Completed(_)
            ));
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while mode.active_workers() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(store.inner.begin("image/png").await.is_ok());
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
    }
}
