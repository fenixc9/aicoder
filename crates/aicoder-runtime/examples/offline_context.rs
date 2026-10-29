//! Select and execute a branch projection, then resume durably without saving repairs as facts.
use aicoder_core::{model::*, session::*, *};
use aicoder_runtime::session::JsonlSessionRepository;
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::json;
use std::{
    fs,
    sync::{Arc, Mutex},
};
struct Offline;
#[async_trait]
impl ModelProvider for Offline {
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
        assert!(
            !request
                .messages
                .iter()
                .any(|message| message.text_content().contains("SIBLING_ONLY"))
        );
        assert!(request.messages.iter().any(|message| {
            message.tool_result().is_some_and(|(id, value, _)| {
                id == "missing" && value["error"]["code"] == "execution_interrupted"
            })
        }));
        assert!(request.messages.iter().any(|message| {
            message
                .text_content()
                .contains("Previous execution was interrupted")
        }));
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "context handled"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let storage = tempfile::tempdir()?;
    let repo = JsonlSessionRepository::new(storage.path())?;
    let mut session = repo.create(workspace.path())?;
    repo.append(&mut session, Message::text(MessageRole::User, "root"))?;
    let root = session.metadata().leaf_id.clone();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::TurnStart {
                turn_id: "crashed".into(),
            },
            SessionEntryData::Message {
                message: Message {
                    role: MessageRole::Assistant,
                    content: ["missing", "done"]
                        .into_iter()
                        .map(|id| ContentBlock::ToolCall {
                            call: ModelToolCall {
                                id: id.into(),
                                name: "work".into(),
                                arguments: json!({}),
                            },
                        })
                        .collect(),
                },
            },
            SessionEntryData::Message {
                message: Message {
                    role: MessageRole::Tool,
                    content: vec![ContentBlock::ToolResult {
                        call_id: "done".into(),
                        content: json!({"ok":true}),
                        is_error: false,
                    }],
                },
            },
        ],
    )?;
    let partial = session.metadata().leaf_id.clone();
    repo.checkout(&mut session, root.as_deref())?;
    repo.append(
        &mut session,
        Message::text(MessageRole::User, "SIBLING_ONLY"),
    )?;
    let before = session.metadata().clone();
    let bytes = fs::read(session.path().unwrap())?;
    let frames = Arc::new(Mutex::new(Vec::new()));
    let captured = frames.clone();
    let executor = TurnExecutor::builder(Offline)
        .workspace(workspace.path())
        .build()?;
    let branch = session.branch(partial.as_deref())?;
    let view = branch.model_context()?;
    ensure!(view.source.recovered_tool_results == 1 && view.origins.len() == view.messages.len());
    let input = ExecutionInput::from_branch(
        ModelRequest::new(
            "offline",
            vec![
                Message::text(MessageRole::System, "trusted"),
                Message::text(MessageRole::User, "dry run"),
            ],
        ),
        &branch,
    )?;
    let outcome = executor
        .run_input_outcome(
            input,
            Arc::new(move |frame: &AgentRawEventEnvelope| {
                captured.lock().unwrap().push(frame.clone())
            }),
            TurnExecutionContext::new(),
        )
        .await;
    let result = outcome.into_result()?;
    ensure!(
        repo.open(&before.id)?.metadata() == &before && fs::read(session.path().unwrap())? == bytes
    );
    ensure!(!result.messages.iter().any(|message| {
        message
            .tool_result()
            .is_some_and(|(id, _, _)| id == "missing")
    }));
    let mut validator = events::EventTraceValidator::new();
    for frame in frames.lock().unwrap().iter() {
        validator.observe(frame)?;
    }
    validator.finish()?;
    if let Some(path) = std::env::args().nth(1) {
        fs::write(
            path,
            frames
                .lock()
                .unwrap()
                .iter()
                .map(|frame| serde_json::to_string(frame).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )?;
    }
    repo.checkout(&mut session, partial.as_deref())?;
    let agent = Agent::new(
        TurnExecutor::builder(Offline)
            .workspace(workspace.path())
            .build()?,
        AgentConfig::new("offline").system_prompt("trusted"),
    );
    let resumed = agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(before.id.clone()),
            "continue",
            Arc::new(()),
        )
        .await?;
    let stored = repo.open(&before.id)?;
    ensure!(stored.entries().iter().any(|entry|matches!(&entry.data,SessionEntryData::Message{message} if message.text_content()=="SIBLING_ONLY")));
    let log = fs::read_to_string(stored.path().unwrap())?;
    ensure!(
        !log.contains("execution_interrupted")
            && !log.contains("Previous execution was interrupted")
    );
    println!(
        "{}",
        json!({"status":"completed","branch_query_is_read_only":true,"canonical_history_kept":true,"recovery_is_model_only":true,"context":resumed.execution_result.model_context})
    );
    Ok(())
}
