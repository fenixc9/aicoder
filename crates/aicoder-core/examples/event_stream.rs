//! Canonical JSONL event subscription, entirely offline. Use --overflow to simulate a slow client.
use aicoder_core::{events::*, model::*, session::MemorySessionRepository, tools::ToolRegistry, *};
use async_trait::async_trait;
use std::sync::Arc;
struct Offline;
#[async_trait]
impl ModelProvider for Offline {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        Ok(ModelSpec::new("offline", model))
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let overflow = std::env::args().any(|arg| arg == "--overflow");
    let agent = Agent::new(
        TurnExecutor::builder(Offline)
            .workspace(std::env::current_dir()?)
            .registry(ToolRegistry::default())
            .build()?,
        AgentConfig::new("offline"),
    );
    let (handle, control) = RunHandle::pair();
    let mut events = handle.subscribe(EventBufferConfig {
        max_events: if overflow { 1 } else { 1024 },
        ..Default::default()
    })?;
    let mut run = tokio::spawn(async move {
        agent
            .run_with_session_controlled(
                &MemorySessionRepository::new(),
                SessionSelection::New,
                "hello",
                Arc::new(()),
                control,
            )
            .await
    });
    let mut outcome = if overflow {
        Some((&mut run).await?)
    } else {
        None
    };
    let observation_error = loop {
        match events.recv().await {
            Ok(Some(event)) => println!("{}", serde_json::to_string(event.as_ref())?),
            Ok(None) => break None,
            Err(error) => break Some(error),
        }
    };
    if outcome.is_none() {
        outcome = Some(run.await?);
    }
    let outcome = outcome.unwrap();
    eprintln!(
        "{}",
        serde_json::json!({"status":if matches!(&outcome,AgentTurnOutcome::Completed(_)) {"completed"} else {"interrupted"},"event_delivery":outcome.progress().event_delivery,"observation_error":observation_error})
    );
    anyhow::ensure!(
        observation_error.is_some() == overflow,
        "unexpected observation result"
    );
    outcome.into_result()?;
    Ok(())
}
