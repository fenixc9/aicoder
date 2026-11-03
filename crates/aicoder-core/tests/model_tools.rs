use aicoder_core::{model::*, session::*, tools::*, *};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
struct Tool {
    name: &'static str,
    prepare: Arc<AtomicUsize>,
    execute: Arc<AtomicUsize>,
    mutating: bool,
}
#[async_trait]
impl ExecutableTool for Tool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        if self.mutating {
            ToolCapability::Mutating
        } else {
            ToolCapability::ReadOnly
        }
    }
    async fn prepare(
        &self,
        _: &ToolContext,
        _: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        self.prepare.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        self.execute.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess::new(json!({"done":true})))
    }
}
struct Provider {
    requests: Mutex<Vec<ModelRequest>>,
    calls: Vec<String>,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        _: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        request.validate(&self.model_spec(&request.model)?)?;
        let first = !request.messages.iter().any(|m| m.role == MessageRole::Tool);
        self.requests.lock().unwrap().push(request.clone());
        Ok(ModelResponse {
            id: None,
            model: request.model,
            message: if first {
                Message {
                    role: MessageRole::Assistant,
                    content: self
                        .calls
                        .iter()
                        .enumerate()
                        .map(|(i, name)| ContentBlock::ToolCall {
                            call: ModelToolCall {
                                id: i.to_string(),
                                name: name.clone(),
                                arguments: json!({}),
                            },
                        })
                        .collect(),
                }
            } else {
                Message::text(MessageRole::Assistant, "done")
            },
            stop_reason: if first {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage::zero(),
        })
    }
}
fn registry() -> (ToolRegistry, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let mut registry = ToolRegistry::default();
    let hidden_prepare = Arc::new(AtomicUsize::new(0));
    let hidden_execute = Arc::new(AtomicUsize::new(0));
    registry
        .register(Tool {
            name: "visible",
            prepare: Arc::new(AtomicUsize::new(0)),
            execute: Arc::new(AtomicUsize::new(0)),
            mutating: false,
        })
        .unwrap();
    registry
        .register(Tool {
            name: "hidden",
            prepare: hidden_prepare.clone(),
            execute: hidden_execute.clone(),
            mutating: true,
        })
        .unwrap();
    (registry, hidden_prepare, hidden_execute)
}
#[tokio::test]
async fn declared_catalog_rejects_hidden_calls_before_preparation_and_records_paired_failures() {
    let root = tempfile::tempdir().unwrap();
    let (registry, prepare, execute) = registry();
    let provider = Arc::new(Provider {
        requests: Mutex::new(vec![]),
        calls: vec!["visible".into(), "hidden".into(), "unknown".into()],
    });
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .registry(registry)
        .model_tools(vec!["visible".into()])
        .approval(AllowAllApproval)
        .build()
        .unwrap();
    assert!(executor.tool_registry().get("hidden").is_some());
    assert_eq!(executor.model_tool_definitions().len(), 1);
    let agent = Agent::new(executor, AgentConfig::new("fixture"));
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let result = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "test",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(result.execution_result.tool_calls, 3);
    assert_eq!(prepare.load(Ordering::SeqCst), 0);
    assert_eq!(execute.load(Ordering::SeqCst), 0);
    let records = repo.open(&session.metadata().id).unwrap();
    let projected = records.selected_branch().model_context().unwrap();
    assert_eq!(projected.source.recovered_tool_results, 0);
    let results = projected
        .messages
        .iter()
        .filter_map(Message::tool_result)
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 3);
    for (_, content, error) in &results[1..2] {
        assert!(*error);
        assert_eq!(content["error"]["code"], "tool_not_exposed");
        assert_eq!(content["error"]["details"]["operation_completed"], false);
    }
    assert_eq!(results[2].1["error"]["code"], "unknown_tool");
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| r.tools.len() == 1 && r.tools[0].name == "visible")
    );
}
#[tokio::test]
async fn per_request_subsets_and_empty_host_catalogs_cannot_fall_back_to_hidden_registry_tools() {
    for empty in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (registry, prepare, execute) = registry();
        let provider = Arc::new(Provider {
            requests: Mutex::new(vec![]),
            calls: vec!["hidden".into()],
        });
        let visible = registry
            .definitions()
            .into_iter()
            .find(|d| d.name == "visible")
            .unwrap();
        let builder = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .registry(registry)
            .approval(AllowAllApproval);
        let executor = if empty {
            builder.model_tools(vec![])
        } else {
            builder
        }
        .build()
        .unwrap();
        let mut request =
            ModelRequest::new("fixture", vec![Message::text(MessageRole::User, "test")]);
        if !empty {
            request.tools = vec![visible];
        }
        let result = executor.run(request).await.unwrap();
        let failure = result
            .messages
            .iter()
            .find_map(Message::tool_result)
            .unwrap();
        assert_eq!(failure.1["error"]["code"], "tool_not_exposed");
        assert_eq!(prepare.load(Ordering::SeqCst), 0);
        assert_eq!(execute.load(Ordering::SeqCst), 0);
        assert_eq!(
            provider.requests.lock().unwrap()[0].tools.len(),
            usize::from(!empty)
        );
    }
}
#[test]
fn model_catalogs_validate_host_names_and_request_definitions_before_model_io() {
    let root = tempfile::tempdir().unwrap();
    for names in [
        vec!["missing".into()],
        vec!["visible".into(), "visible".into()],
        vec!["x".repeat(129)],
    ] {
        let (registry, _, _) = registry();
        assert!(
            TurnExecutor::builder(Provider {
                requests: Mutex::new(vec![]),
                calls: vec![]
            })
            .workspace(root.path())
            .registry(registry)
            .model_tools(names)
            .build()
            .is_err()
        );
    }
    let (registry, _, _) = registry();
    let hidden = registry
        .definitions()
        .into_iter()
        .find(|d| d.name == "hidden")
        .unwrap();
    let provider = Arc::new(Provider {
        requests: Mutex::new(vec![]),
        calls: vec![],
    });
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .registry(registry)
        .model_tools(vec!["visible".into()])
        .build()
        .unwrap();
    let mut request = ModelRequest::new("fixture", vec![Message::text(MessageRole::User, "test")]);
    request.tools = vec![hidden];
    assert!(
        executor
            .validate_model_selection(&ExecutionInput::from_request(request))
            .is_err()
    );
    assert!(provider.requests.lock().unwrap().is_empty());
}
