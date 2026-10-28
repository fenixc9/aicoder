//! Deterministic end-to-end smoke and small runtime baseline; never calls a model service.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use aicoder_core::{
    Agent, AgentConfig, AgentRawEventEnvelope, AgentTurnOutcome, ChatCompletionProvider,
    SessionSelection, TurnExecutionContext, TurnExecutor,
    session::{JsonlSessionRepository, SessionRepository},
    tools::AllowAllApproval,
    types::{ChatCompletionRequest, ChatCompletionResponse, Role},
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
impl ChatCompletionProvider for ScriptedProvider {
    async fn complete(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        let last = request.messages.last().expect("smoke sends a user message");
        if matches!(self.case, Case::Cancel) {
            self.cancellation.cancel("offline smoke cancellation");
            return std::future::pending().await;
        }
        let mut message = json!({"role": "assistant", "content": "done"});
        let mut finish_reason = "stop";
        if matches!(last.role, Role::Tool) {
            let output: Value = serde_json::from_str(last.content.as_deref().unwrap())?;
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
            if last.content.as_deref() == Some("resume") {
                ensure!(
                    request
                        .messages
                        .iter()
                        .any(|m| m.content.as_deref() == Some("remember fixture")),
                    "resumed history missing"
                );
                ensure!(
                    request
                        .messages
                        .iter()
                        .any(|m| matches!(m.role, Role::Assistant)),
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
            message = json!({"role":"assistant", "tool_calls":[{
                "id":"smoke-call", "type":"function",
                "function":{"name":tool, "arguments":arguments.to_string()}
            }]});
            finish_reason = "tool_calls";
        }
        Ok(serde_json::from_value(json!({
            "id":"smoke-response", "object":"chat.completion", "created":0,
            "model":request.model, "choices":[{"index":0, "message":message, "finish_reason":finish_reason}]
        }))?)
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
        result.final_message.content.as_deref() == Some("done"),
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
