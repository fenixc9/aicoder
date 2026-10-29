//! A full agent turn with an in-memory provider, tool, store and host-owned event sink.
use aicoder_core::{
    Agent, AgentConfig, ModelProvider, SessionSelection, TurnExecutionContext, TurnExecutor,
    model::*,
    session::{MemorySessionRepository, SessionRepository},
    tools::{
        AllowAllApproval, ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolRegistry,
        ToolSuccess,
    },
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

struct Counter(Arc<AtomicU64>);
#[async_trait]
impl ExecutableTool for Counter {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "increment".into(),
            description: Some("Increment an in-memory counter".into()),
            parameters: json!({"type":"object","properties":{"amount":{"type":"integer","minimum":1}},"required":["amount"]}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        let amount = arguments["amount"]
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or_else(|| ToolFailure {
                artifacts: Vec::new(),
                code: "invalid_arguments".into(),
                message: "amount must be positive".into(),
                details: None,
            })?;
        let value = self
            .0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(amount)
            })
            .map_err(|_| ToolFailure {
                artifacts: Vec::new(),
                code: "overflow".into(),
                message: "counter overflow".into(),
                details: None,
            })?
            + amount;
        Ok(ToolSuccess {
            artifacts: Vec::new(),
            output: json!({"value":value}),
            truncated: false,
        })
    }
}

struct InMemoryModel;
#[async_trait]
impl ModelProvider for InMemoryModel {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("memory", model);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        events: Arc<dyn ModelEventSink>,
        context: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let validation = if context.is_cancelled() {
            Err(ProviderError::new(
                "memory",
                ProviderErrorKind::Cancelled,
                context.reason(),
            ))
        } else {
            self.model_spec(&request.model)
                .and_then(|spec| request.validate(&spec))
        };
        if let Err(error) = validation {
            events.emit(ModelEvent::Aborted {
                message: error.to_string(),
            });
            return Err(error);
        }
        let (message, stop_reason) =
            if let Some((_, output, _)) = request.messages.last().and_then(Message::tool_result) {
                (
                    Message::text(
                        MessageRole::Assistant,
                        format!("count={}", output["output"]["value"]),
                    ),
                    StopReason::Complete,
                )
            } else {
                (
                    Message {
                        role: MessageRole::Assistant,
                        content: vec![ContentBlock::ToolCall {
                            call: ModelToolCall {
                                id: "memory-call".into(),
                                name: "increment".into(),
                                arguments: json!({"amount":2}),
                            },
                        }],
                    },
                    StopReason::ToolCalls,
                )
            };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason,
            usage: ModelUsage::default(),
        };
        emit_response(&response, events.as_ref());
        Ok(response)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let counter = Arc::new(AtomicU64::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Counter(counter.clone()))?;
    // The workspace scopes tool context; this example performs no file/command/model-service I/O.
    let executor = TurnExecutor::builder(InMemoryModel)
        .workspace(std::env::current_dir()?)
        .registry(tools)
        .approval(AllowAllApproval)
        .build()?;
    let agent = Agent::new(executor, AgentConfig::new("counter"));
    let sessions = MemorySessionRepository::new();
    let result = agent
        .run_with_session(&sessions, SessionSelection::New, "increment", Arc::new(()))
        .await?;
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    assert_eq!(
        sessions.open(&result.session.unwrap().id)?.messages().len(),
        4
    );
    assert_eq!(result.execution_result.usage.total_tokens, None);
    println!("{}", result.execution_result.final_message.text_content());
    Ok(())
}
