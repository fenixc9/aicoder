//! Durable semantic compaction with a scripted provider; no network or credentials.
use aicoder_core::{context::*, model::*, session::*, summary::*, *};
use aicoder_runtime::session::JsonlSessionRepository;
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::json;
use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
#[derive(Default)]
struct Offline {
    summaries: AtomicUsize,
}
#[async_trait]
impl ModelProvider for Offline {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.context_tokens = Some(16384);
        spec.max_output_tokens = Some(1024);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let summarizing = request.messages[0]
            .text_content()
            .starts_with("Summarize historical");
        let message = if summarizing {
            self.summaries.fetch_add(1, Ordering::SeqCst);
            ensure_summary_request(&request);
            json!({"goals":["finish feature"],"constraints":["preserve public API"],"changes":["src/lib.rs updated"],"todos":["run tests"],"notes":[]}).to_string()
        } else {
            assert!(
                request
                    .messages
                    .iter()
                    .any(|m| m.text_content().contains("preserve public API"))
            );
            "done".into()
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, message),
            stop_reason: StopReason::Complete,
            usage: ModelUsage {
                input_tokens: Some(8),
                output_tokens: Some(2),
                total_tokens: Some(10),
                ..Default::default()
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
fn ensure_summary_request(request: &ModelRequest) {
    assert!(request.tools.is_empty());
    assert_eq!(request.tool_choice, ModelToolChoice::None);
}
fn compactor(provider: Arc<Offline>) -> Result<SemanticContextCompactor> {
    let mut config = SummaryConfig::new(ContextWindowConfig {
        max_context_tokens: 1200,
        reserved_output_tokens: 200,
        preserve_recent_tokens: 80,
    });
    config.output_tokens = 128;
    config.max_input_tokens = 1400;
    SemanticContextCompactor::new(provider, "offline", config)
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let storage = tempfile::tempdir()?;
    let repo = JsonlSessionRepository::new(storage.path())?;
    let mut session = repo.create(workspace.path())?;
    for i in 0..8 {
        repo.append(
            &mut session,
            Message::text(
                if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                format!("historical {i}: {}", "x".repeat(1000)),
            ),
        )?;
    }
    repo.append(&mut session, Message::text(MessageRole::User, "latest"))?;
    let raw = session.selected_branch().chat_messages();
    let original_leaf = session.metadata().leaf_id.clone();
    let provider = Arc::new(Offline::default());
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(workspace.path())
            .context_compactor(compactor(provider.clone())?)
            .build()?,
        AgentConfig::new("offline").system_prompt("trusted prefix"),
    );
    let report = agent
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    ensure!(
        report.succeeded() && report.checkpoint_entry_id.is_some(),
        "{report:?}"
    );
    let saved = repo.open(&session.metadata().id)?;
    ensure!(saved.selected_branch().chat_messages() == raw);
    ensure!(
        saved
            .selected_branch()
            .model_context()?
            .source
            .summary_checkpoint
            == report.checkpoint_entry_id
    );
    ensure!(
        saved
            .branch(original_leaf.as_deref())?
            .model_context()?
            .messages
            == raw
    );
    ensure!(saved.selected_branch().accounting()?.usage == report.usage);
    let summaries = provider.summaries.load(Ordering::SeqCst);
    let frames = Arc::new(Mutex::new(Vec::new()));
    let output = frames.clone();
    let observer: Arc<dyn AgentEventHandler> =
        Arc::new(move |frame: &AgentRawEventEnvelope| output.lock().unwrap().push(frame.clone()));
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "continue",
            observer,
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    ensure!(
        provider.summaries.load(Ordering::SeqCst) == summaries,
        "resume must reuse checkpoint"
    );
    let restored = repo.open(&session.metadata().id)?;
    ensure!(restored.messages().len() == raw.len() + 2);
    ensure!(
        restored.selected_branch().accounting()?.usage.total_tokens
            == Some(report.usage.total_tokens.unwrap() + 10)
    );
    let frames = frames.lock().unwrap();
    let mut validator = aicoder_core::events::EventTraceValidator::new();
    for frame in frames.iter() {
        validator.observe(frame)?;
    }
    validator.finish()?;
    if let Some(path) = std::env::args().nth(1) {
        fs::write(
            path,
            frames
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()?
                .join("\n")
                + "\n",
        )?;
    }
    println!(
        "{}",
        json!({"status":"completed","summary_attempts":summaries,"manual":report,"raw_history_preserved":true,"branch_isolated":true,"resume_reused_checkpoint":true,"durable_usage":restored.selected_branch().accounting()?})
    );
    Ok(())
}
