use aicoder_core::{model::*, *};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
struct Waiting(Mutex<Option<tokio::sync::oneshot::Sender<()>>>);
#[async_trait]
impl ModelProvider for Waiting {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.context_tokens = Some(32000);
        Ok(spec)
    }
    async fn generate(
        &self,
        _: ModelRequest,
        _: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.0.lock().unwrap().take().unwrap().send(()).unwrap();
        std::future::pending().await
    }
}
#[tokio::test]
async fn a_live_handle_exposes_counted_projected_context_before_response_without_prompt_data() {
    let workspace = tempfile::tempdir().unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let agent = Agent::new(
        TurnExecutor::builder(Waiting(Mutex::new(Some(entered))))
            .workspace(workspace.path())
            .build()
            .unwrap(),
        AgentConfig::new("m1"),
    );
    let (handle, control) = RunHandle::pair();
    let owner = tokio::spawn(async move {
        agent
            .run_controlled("private user prompt", Arc::new(()), control)
            .await
    });
    ready.await.unwrap();
    let info = handle.model_context().unwrap();
    let count = info.last_input_count.as_ref().unwrap();
    assert_eq!(info.requests, 1);
    assert!(count.tokens > 0);
    assert_eq!(count.scope, aicoder_core::tokens::TokenScope::Request);
    assert_eq!(handle.snapshot().model_context.as_ref(), Some(&info));
    assert!(
        !serde_json::to_string(&info)
            .unwrap()
            .contains("private user prompt")
    );
    handle.cancel("test").unwrap();
    assert!(matches!(owner.await.unwrap(), AgentTurnOutcome::Aborted(_)));
}
