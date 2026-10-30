//! Validate, persist and replay branch reasoning without a service, key or tool execution.
use aicoder_core::{model::*, session::*, *};
use aicoder_runtime::session::JsonlSessionRepository;
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};
struct Provider(Arc<Mutex<Vec<Option<ReasoningEffort>>>>);
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.reasoning_efforts = vec![
            ReasoningEffort::None,
            ReasoningEffort::Low,
            ReasoningEffort::High,
        ];
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.0
            .lock()
            .unwrap()
            .push(request.options.reasoning_effort);
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
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(event.clone());
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let storage = tempfile::tempdir()?;
    let repo = JsonlSessionRepository::new(storage.path())?;
    let session = repo.create(workspace.path())?;
    let id = session.metadata().id.clone();
    let calls = Arc::new(Mutex::new(vec![]));
    let agent = Agent::new(
        TurnExecutor::builder(Provider(calls.clone()))
            .workspace(workspace.path())
            .build()?,
        AgentConfig::new("m1"),
    )
    .following_session_models()
    .following_session_reasoning();
    agent
        .switch_session_reasoning(
            &repo,
            &id,
            Some(ReasoningEffort::High),
            TurnExecutionContext::new(),
        )
        .await?;
    let events = Arc::new(Events::default());
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            "first",
            events.clone(),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    let first = events.0.lock().unwrap().clone();
    let mut validator = aicoder_core::events::EventTraceValidator::new();
    for event in &first {
        validator.observe(event)?;
    }
    validator.finish()?;
    agent
        .switch_session_model_with_reasoning(
            &repo,
            &id,
            "m2",
            Some(ReasoningEffort::Low),
            TurnExecutionContext::new(),
        )
        .await?;
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            "second",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    agent
        .switch_session_model(&repo, &id, "m1", TurnExecutionContext::new())
        .await?;
    let final_state = repo.open(&id)?;
    anyhow::ensure!(
        final_state.reasoning_selection()?.is_none(),
        "old model effort leaked across epochs"
    );
    let levels = calls.lock().unwrap();
    anyhow::ensure!(
        *levels == vec![Some(ReasoningEffort::High), Some(ReasoningEffort::Low)],
        "requested levels changed"
    );
    if let Some(path) = std::env::args_os().nth(1) {
        let mut out = std::fs::File::create(path)?;
        for event in &first {
            serde_json::to_writer(&mut out, event)?;
            std::io::Write::write_all(&mut out, b"\n")?;
        }
    }
    println!(
        "{}",
        json!({"requested_efforts":*levels,"selected_model":final_state.selected_model(),"old_effort_reused":false,"coding_requests":levels.len(),"event_frames":first.len()})
    );
    Ok(())
}
