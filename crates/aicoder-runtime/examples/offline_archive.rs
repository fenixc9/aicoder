//! Generate outside the measured reader process, then continue from a bounded archive window.
use aicoder_core::{context::*, model::*, session::*, *};
use aicoder_runtime::session::{JsonlSessionRepository, SessionLoadLimits};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::json;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
struct Offline(Arc<Mutex<usize>>);
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
        *self.0.lock().unwrap() += 1;
        ensure_request(&request);
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
fn ensure_request(request: &ModelRequest) {
    assert!(estimate_context_tokens(&request.messages, Some(&request.tools)) <= 896);
    assert!(
        !request
            .messages
            .iter()
            .any(|m| m.text_content().contains("ARCHIVED_PAYLOAD"))
    );
}
fn generate(root: &Path, messages: usize) -> Result<()> {
    std::fs::create_dir_all(root.join("workspace"))?;
    let repo = JsonlSessionRepository::new(root.join("sessions"))?;
    let mut session = repo.create(&root.join("workspace"))?;
    for start in (0..messages).step_by(64) {
        repo.append_all(
            &mut session,
            (start..(start + 64).min(messages)).map(|_| {
                Message::text(
                    MessageRole::User,
                    format!("ARCHIVED_PAYLOAD {}", "x".repeat(16384)),
                )
            }),
        )?;
    }
    repo.append(&mut session, Message::text(MessageRole::User, "latest"))?;
    std::fs::write(root.join("id"), &session.metadata().id)?;
    Ok(())
}
async fn run(root: &Path) -> Result<()> {
    let id = std::fs::read_to_string(root.join("id"))?;
    let repo = JsonlSessionRepository::with_limits(
        root.join("sessions"),
        SessionLoadLimits {
            max_snapshot_bytes: 1024 * 1024,
            max_window_bytes: 256 * 1024,
            ..Default::default()
        },
    )?;
    let state = repo.open_run(&id)?;
    let loaded = state.messages().len();
    let (source, _) = state.source_and_context()?;
    ensure!(source.archived_messages > 0);
    let archive_bytes = std::fs::metadata(repo.root().join(format!("{id}.jsonl")))
        .ok()
        .map(|m| m.len());
    let requests = Arc::new(Mutex::new(0));
    let agent = Agent::new(
        TurnExecutor::builder(Offline(requests.clone()))
            .workspace(root.join("workspace"))
            .context_compactor(PruningContextCompactor::new(ContextWindowConfig {
                max_context_tokens: 1024,
                reserved_output_tokens: 128,
                preserve_recent_tokens: 32,
            })?)
            .build()?,
        AgentConfig::new("offline"),
    );
    let frames = Arc::new(Mutex::new(Vec::new()));
    let captured = frames.clone();
    let result = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            "continue",
            Arc::new(move |frame: &AgentRawEventEnvelope| {
                captured.lock().unwrap().push(frame.clone())
            }),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    let mut validator = events::EventTraceValidator::new();
    for frame in frames.lock().unwrap().iter() {
        validator.observe(frame)?;
    }
    validator.finish()?;
    ensure!(
        repo.archive_page(&id, 1, None)?.entries[0].id == state.messages()[0].id
            || loaded < source.recorded_messages
    );
    ensure!(*requests.lock().unwrap() == 1);
    println!(
        "{}",
        json!({"status":"completed","archive_bytes":archive_bytes,"total_messages_before":source.recorded_messages,"loaded_messages":loaded,"archived_messages":source.archived_messages,"context":result.execution_result.model_context,"event_frames":frames.lock().unwrap().len(),"limits":repo.limits()})
    );
    Ok(())
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--generate") {
        generate(
            Path::new(&args[1]),
            args.get(2).map_or(Ok(4096), |arg| arg.parse())?,
        )?;
    } else if let Some(root) = args.first() {
        run(Path::new(root)).await?;
    } else {
        let root = tempfile::tempdir()?;
        generate(root.path(), 128)?;
        run(root.path()).await?;
    }
    Ok(())
}
