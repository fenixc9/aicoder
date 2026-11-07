//! Native idle edits over full snapshots or bounded windows, followed by the ordinary Agent loop.
use aicoder_core::{model::*, session::*, *};
use aicoder_runtime::session::{JsonlSessionRepository, SessionLoadLimits};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
#[derive(Clone, Default)]
struct Provider(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.context_tokens = Some(65536);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let response = ModelResponse {
            id: None,
            model: request.model.clone(),
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        self.0.lock().unwrap().push(request);
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let window = std::env::args().any(|arg| arg == "--window");
    let workspace = tempfile::tempdir()?;
    let storage = tempfile::tempdir()?;
    let writer = JsonlSessionRepository::new(storage.path())?;
    let mut s = writer.create(workspace.path())?;
    for i in 0..8 {
        writer.append(
            &mut s,
            Message::text(MessageRole::User, format!("original-{i}")),
        )?;
    }
    let target = s.metadata().leaf_id.clone().unwrap();
    let repo = JsonlSessionRepository::with_limits(
        storage.path(),
        SessionLoadLimits {
            max_snapshot_bytes: if window { 512 } else { 16 * 1024 * 1024 },
            max_window_messages: if window { 2 } else { 2048 },
            ..Default::default()
        },
    )?;
    let provider = Provider::default();
    let agent = Agent::new(
        TurnExecutor::builder(provider.clone())
            .workspace(workspace.path())
            .build()?,
        AgentConfig::new("fixture"),
    );
    let result = agent
        .navigate_session(
            &repo,
            &s.metadata().id,
            ArchiveNavigation::EditContext {
                edit: ContextEditRecord {
                    version: 1,
                    extension: "example".into(),
                    hook: "edit".into(),
                    target_id: target,
                    replacement: ContextReplacement::Text("model-facing replacement".into()),
                },
            },
            TurnExecutionContext::new(),
        )
        .await?;
    ensure!(
        matches!(result.session, RunSession::Window(_)) == window,
        "unexpected storage mode"
    );
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "continue",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    ensure!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "run failed: {outcome:?}"
    );
    let requests = provider.0.lock().unwrap();
    ensure!(requests.len() == 1, "duplicate model request");
    ensure!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.text_content() == "model-facing replacement"),
        "edit missing from model view"
    );
    ensure!(
        !requests[0]
            .messages
            .iter()
            .any(|m| m.text_content() == "original-7"),
        "original leaked into edited model view"
    );
    let persisted = writer.open(&s.metadata().id)?;
    ensure!(
        persisted
            .messages()
            .iter()
            .any(|m| m.message.text_content() == "original-7"),
        "canonical target changed"
    );
    ensure!(
        !persisted
            .messages()
            .iter()
            .any(|m| m.message.text_content() == "model-facing replacement"),
        "projection recorded as canonical message"
    );
    println!(
        "{}",
        serde_json::json!({"storage":if window {"window"}else{"full"},"requests":requests.len(),"raw_messages":persisted.messages().len(),"canonical_target_preserved":true})
    );
    Ok(())
}
