//! Deterministic end-to-end smoke and small runtime baseline; never calls a model service.
use aicoder_runtime::{session::JsonlSessionRepository, tools::default_registry};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use aicoder_core::{
    Agent, AgentConfig, AgentRawEventEnvelope, AgentTurnOutcome, ModelProvider, SessionSelection,
    TurnExecutionContext, TurnExecutor,
    model::{
        ContentBlock, Message, MessageRole, ModelRequest, ModelResponse, ModelToolCall, ModelUsage,
        StopReason,
    },
    session::SessionRepository,
    tools::AllowAllApproval,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug)]
enum Case {
    Edit,
    Search,
    ToolFailure,
    Cancel,
    SessionResume,
}

struct ScriptedProvider {
    case: Case,
    cancellation: TurnExecutionContext,
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn model_spec(
        &self,
        model: &str,
    ) -> std::result::Result<aicoder_core::model::ModelSpec, aicoder_core::model::ProviderError>
    {
        let mut spec = aicoder_core::model::ModelSpec::new("scripted", model);
        spec.capabilities = aicoder_core::model::ModelCapabilities {
            reasoning_efforts: Vec::new(),
            tools: true,
            images: true,
            reasoning_history: true,
            temperature: true,
            top_p: true,
            seed: true,
            stop_sequences: true,
            structured_output: true,
        };
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        events: Arc<dyn aicoder_core::model::ModelEventSink>,
        _context: aicoder_core::TurnExecutionContext,
    ) -> std::result::Result<ModelResponse, aicoder_core::model::ProviderError> {
        let result: anyhow::Result<ModelResponse> = async {
        let last = request.messages.last().expect("smoke sends a user message");
        if matches!(self.case, Case::Cancel) {
            self.cancellation.cancel("offline smoke cancellation");
            return std::future::pending().await;
        }
        let mut message = Message::text(MessageRole::Assistant,"done");
        let mut finish_reason = StopReason::Complete;
        if matches!(last.role, MessageRole::Tool) {
            let output = last.tool_result().unwrap().1;
            match self.case {
                Case::Edit => ensure!(output["ok"] == true, "edit failed: {output}"),
                Case::Search => ensure!(
                    output["ok"] == true
                        && output["output"]["matches"]
                            .as_str()
                            .unwrap_or_default()
                            .contains("needle"),
                    "search did not find fixture: {output}"
                ),
                Case::ToolFailure => ensure!(
                    output["error"]["code"] == "unknown_tool",
                    "unexpected failure: {output}"
                ),
                _ => unreachable!(),
            }
        } else if matches!(self.case, Case::SessionResume) {
            if last.text_content().as_str() == "resume" {
                ensure!(
                    request
                        .messages
                        .iter()
                        .any(|m| m.text_content().as_str() == "remember fixture"),
                    "resumed history missing"
                );
                ensure!(
                    request
                        .messages
                        .iter()
                        .any(|m| matches!(m.role, MessageRole::Assistant)),
                    "previous answer missing"
                );
            }
        } else {
            let (tool, arguments) = match self.case {
                Case::Edit => (
                    "edit_file",
                    json!({"path":"fixture.txt", "edits":[{"old_text":"before", "new_text":"after"}]}),
                ),
                Case::Search => ("grep", json!({"pattern":"needle", "path":"fixture.txt"})),
                Case::ToolFailure => ("nonexistent_tool", json!({})),
                _ => unreachable!(),
            };
            message = Message {role:MessageRole::Assistant,content:vec![ContentBlock::ToolCall {call:ModelToolCall {id:"smoke-call".into(),name:tool.into(),arguments}}]};
            finish_reason = StopReason::ToolCalls;
        }
        Ok(ModelResponse {id:Some("smoke-response".into()),model:request.model,message,stop_reason:finish_reason,usage:ModelUsage::default()})
    }.await;
        match result {
            Ok(response) => {
                aicoder_core::model::emit_response(&response, events.as_ref());
                Ok(response)
            }
            Err(error) => Err(aicoder_core::model::ProviderError::new(
                "scripted",
                aicoder_core::model::ProviderErrorKind::Other,
                error.to_string(),
            )),
        }
    }
}

async fn run_case(case: Case) -> Result<Value> {
    let workspace = tempfile::tempdir()?;
    std::fs::write(workspace.path().join("fixture.txt"), "before needle\n")?;
    let context = TurnExecutionContext::new();
    let executor = TurnExecutor::builder(ScriptedProvider {
        case,
        cancellation: context.clone(),
    })
    .registry(default_registry()?)
    .workspace(workspace.path())
    .approval(AllowAllApproval)
    .build()?;
    let agent = Agent::new(executor, AgentConfig::new("offline-scripted"));
    let events = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&events);
    let handler = Arc::new(move |_: &AgentRawEventEnvelope| {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    let started = Instant::now();
    let result = if matches!(case, Case::SessionResume) {
        let storage = tempfile::tempdir()?;
        let repository = JsonlSessionRepository::new(storage.path())?;
        let first = agent
            .run_with_session(
                &repository,
                SessionSelection::New,
                "remember fixture",
                handler.clone(),
            )
            .await?;
        let id = first.session.unwrap().id;
        drop(repository);
        let reopened = JsonlSessionRepository::new(storage.path())?;
        let result = agent
            .run_with_session(
                &reopened,
                SessionSelection::Existing(id.clone()),
                "resume",
                handler,
            )
            .await?;
        ensure!(
            reopened.open(&id)?.messages().len() == 4,
            "unexpected persisted transcript"
        );
        result.execution_result
    } else {
        match agent.run_outcome("run fixture", handler, context).await {
            AgentTurnOutcome::Completed(result) => {
                ensure!(!matches!(case, Case::Cancel), "cancellation was ignored");
                result.execution_result
            }
            AgentTurnOutcome::Aborted(_) if matches!(case, Case::Cancel) => {
                return Ok(
                    json!({"case":format!("{case:?}"), "outcome":"aborted", "events":events.load(Ordering::Relaxed), "elapsed_us":started.elapsed().as_micros()}),
                );
            }
            other => anyhow::bail!("unexpected outcome: {other:?}"),
        }
    };
    ensure!(
        result.final_message.text_content().as_str() == "done",
        "final answer missing"
    );
    if matches!(case, Case::Edit) {
        ensure!(
            std::fs::read_to_string(workspace.path().join("fixture.txt"))? == "after needle\n",
            "file was not edited"
        );
    }
    Ok(json!({
        "case":format!("{case:?}"), "outcome":"completed", "rounds":result.rounds,
        "messages":result.messages.len(), "transcript_json_bytes":serde_json::to_vec(&result.messages)?.len(),
        "events":events.load(Ordering::Relaxed), "elapsed_us":started.elapsed().as_micros()
    }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut iterations = 1_usize;
    let mut idle_ms = 0_u64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iterations" => {
                iterations = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing iterations"))?
                    .parse()?
            }
            "--idle-ms" => {
                idle_ms = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing idle duration"))?
                    .parse()?
            }
            _ => anyhow::bail!("unknown argument: {arg}"),
        }
    }
    ensure!(iterations > 0, "iterations must be positive");
    if idle_ms > 0 {
        tokio::time::sleep(Duration::from_millis(idle_ms)).await;
    }
    let started = Instant::now();
    let mut cases = Vec::new();
    for _ in 0..iterations {
        for case in [
            Case::Edit,
            Case::Search,
            Case::ToolFailure,
            Case::Cancel,
            Case::SessionResume,
        ] {
            cases.push(run_case(case).await?);
        }
    }
    println!(
        "{}",
        serde_json::to_string(&json!({
            "schema_version":1, "provider":"offline-scripted", "iterations":iterations,
            "elapsed_us":started.elapsed().as_micros(), "cases":cases
        }))?
    );
    Ok(())
}
