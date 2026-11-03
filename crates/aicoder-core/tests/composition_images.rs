//! Public composition image contract without a JS engine, codecs or HTTP.
use aicoder_core::{artifacts::*, composition::*, model::*, session::*, tools::*, *};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
struct Capture(usize);
#[async_trait]
impl ExecutableTool for Capture {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "capture".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(&self, ctx: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        let mut result = ToolSuccess::new(json!({"captured":true}));
        let store = ctx.artifact_store().unwrap();
        for _ in 0..self.0 {
            result.artifacts.push(
                store_bytes(store.as_ref(), " IMAGE/PNG; profile=fixture", b"raw", true)
                    .await
                    .unwrap(),
            );
        }
        result.artifacts.push(
            store_bytes(store.as_ref(), "application/json", b"{}", true)
                .await
                .unwrap(),
        );
        Ok(result)
    }
}
struct Program {
    expose: bool,
    stage: u8,
    response: Option<ProgramResponse>,
}
#[async_trait]
impl ToolProgram for Program {
    async fn next(&mut self) -> Result<ProgramEvent, ToolFailure> {
        if self.stage == 0 {
            self.stage = 1;
            let call = ProgramCall {
                id: 1,
                tool: "capture".into(),
                arguments: json!({}),
            };
            return Ok(if self.expose {
                ProgramEvent::ImageCall(call)
            } else {
                ProgramEvent::Call(call)
            });
        }
        if let Some(response) = self.response.take() {
            self.stage = 2;
            return Ok(ProgramEvent::Finished(ProgramOutput {
                result: ToolSuccess::new(
                    json!({"is_error":response.is_error,"code":response.content["error"]["code"]}),
                ),
                state: None,
            }));
        }
        std::future::pending().await
    }
    fn respond(&mut self, response: ProgramResponse) -> Result<(), ToolFailure> {
        self.response = Some(response);
        Ok(())
    }
    fn cancel(&mut self) {}
    async fn shutdown(&mut self) -> Result<(), ToolFailure> {
        Ok(())
    }
}
struct Factory(bool);
impl ToolCompositionFactory for Factory {
    fn allowed_tools(&self) -> Option<&[String]> {
        None
    }
    fn start(&self, _: CompositionContext, _: Value) -> Result<Box<dyn ToolProgram>, ToolFailure> {
        Ok(Box::new(Program {
            expose: self.0,
            stage: 0,
            response: None,
        }))
    }
}
#[async_trait]
impl ExecutableTool for Factory {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "program".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Command
    }
    fn composition_factory(&self) -> Option<&dyn ToolCompositionFactory> {
        Some(self)
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        panic!("core must use composition")
    }
}
struct Provider(Mutex<usize>);
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
        let mut calls = self.0.lock().unwrap();
        *calls += 1;
        let first = *calls == 1;
        Ok(ModelResponse {
            id: None,
            model: request.model,
            message: if first {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "outer".into(),
                            name: "program".into(),
                            arguments: json!({}),
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
            usage: ModelUsage::zero(),
        })
    }
}
async fn run(expose: bool, count: usize) -> (Vec<Message>, Vec<Message>) {
    let root = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::default();
    registry.register(Capture(count)).unwrap();
    registry.register(Factory(expose)).unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(Provider(Mutex::new(0)))
            .workspace(root.path())
            .registry(registry)
            .artifact_store(Arc::new(MemoryArtifactStore::default()))
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let result = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "run",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(result.execution_result.tool_calls, 2);
    let session = repo.open(&session.metadata().id).unwrap();
    let leaves = session
        .entries()
        .iter()
        .filter_map(|entry| match &entry.data {
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
        .collect();
    (result.execution_result.messages.clone(), leaves)
}
#[tokio::test]
async fn only_explicit_image_calls_reach_outer_results_while_recovery_retains_nonimages() {
    for expose in [false, true] {
        let (messages, leaves) = run(expose, 1).await;
        assert_eq!(
            leaves[0]
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::Image { .. }))
                .count(),
            1
        );
        let root = messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .unwrap();
        let result = root.tool_result().unwrap().1;
        assert_eq!(
            root.content
                .iter()
                .filter(|b| matches!(b, ContentBlock::Image { .. }))
                .count(),
            usize::from(expose)
        );
        assert_eq!(
            result["artifacts"].as_array().unwrap().len(),
            if expose { 2 } else { 1 }
        );
        assert_eq!(result["output"]["program"]["is_error"], false);
    }
}
#[tokio::test]
async fn ambiguous_explicit_image_results_preserve_real_leaf_facts_and_do_not_publish_images() {
    let (messages, leaves) = run(true, 2).await;
    assert_eq!(leaves[0].content.len(), 3);
    let root = messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .unwrap();
    assert_eq!(root.content.len(), 1);
    let result = root.tool_result().unwrap().1;
    assert_eq!(
        result["output"]["program"]["code"],
        "composition_image_result"
    );
    assert_eq!(result["artifacts"].as_array().unwrap().len(), 1);
}
