//! Run a controlled agent with follow-up and asynchronous tool approval, entirely offline.
use aicoder_core::{model::*, tools::*, *};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Script(AtomicUsize);
#[async_trait]
impl ModelProvider for Script {
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
        let step = self.0.fetch_add(1, Ordering::SeqCst);
        let message = if step == 0 {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "remember-1".into(),
                        name: "remember".into(),
                        arguments: json!({}),
                    },
                }],
            }
        } else {
            Message::text(
                MessageRole::Assistant,
                if step == 1 {
                    "Remembered after approval."
                } else {
                    "Follow-up completed."
                },
            )
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if step == 0 {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Remember(Arc<AtomicUsize>);
#[async_trait]
impl ExecutableTool for Remember {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "remember".into(),
            description: Some("Increment an in-memory counter".into()),
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess {
            artifacts: Vec::new(),
            output: json!("recorded in memory"),
            truncated: false,
        })
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let remembered = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry.register(Remember(remembered.clone()))?;
    let executor = TurnExecutor::builder(Script(AtomicUsize::new(0)))
        .workspace(std::env::current_dir()?)
        .registry(registry)
        .build()?;
    let (handle, control) = RunHandle::channel(
        TurnExecutionContext::new(),
        RunControlConfig {
            handle_approvals: true,
            ..Default::default()
        },
    )?;
    handle.follow_up(Message::text(MessageRole::User, "Explain what happened."))?;
    let run = tokio::spawn(async move {
        executor
            .run_controlled(
                ModelRequest::new(
                    "script",
                    vec![Message::text(
                        MessageRole::User,
                        "Remember this with my approval.",
                    )],
                ),
                Arc::new(()),
                control,
            )
            .await
    });
    loop {
        let snapshot = handle.snapshot();
        if let Some(approval) = snapshot.approvals.first() {
            handle.respond_approval(&approval.id, true)?;
            break;
        }
        anyhow::ensure!(
            !matches!(
                snapshot.status,
                RunControlStatus::Finishing
                    | RunControlStatus::Finished
                    | RunControlStatus::Dropped
            ),
            "run ended before requesting approval"
        );
        handle.changed(snapshot.revision).await;
    }
    let result = run.await?.into_result()?;
    anyhow::ensure!(
        result.rounds == 3
            && remembered.load(Ordering::SeqCst) == 1
            && result.pending_inputs.is_empty(),
        "unexpected controlled run"
    );
    println!(
        "{}",
        json!({"status":"completed","rounds":result.rounds,"approved_operations":remembered.load(Ordering::SeqCst),"pending_inputs":result.pending_inputs.len()})
    );
    Ok(())
}
