//! Persistent conversation, transient tool grants, reuse and revocation through RunHandle.
use aicoder_core::{model::*, tools::*, *};
use aicoder_runtime::{session::JsonlSessionRepository, tools::default_registry};
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
        let done = request
            .messages
            .last()
            .is_some_and(|message| message.role == MessageRole::Tool);
        let message = if done {
            Message::text(MessageRole::Assistant, "permission flow handled")
        } else {
            let content = request.messages.last().unwrap().text_content();
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: format!("write-{content}"),
                        name: "write_file".into(),
                        arguments: json!({"path":"file.txt","content":content}),
                    },
                }],
            }
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if done {
                StopReason::Complete
            } else {
                StopReason::ToolCalls
            },
            usage: ModelUsage::default(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let storage = tempfile::tempdir()?;
    let repository = Arc::new(JsonlSessionRepository::new(storage.path())?);
    let cache = ApprovalSessions::default();
    let agent = Arc::new(Agent::new(
        TurnExecutor::builder(Offline)
            .workspace(workspace.path())
            .registry(default_registry()?)
            .build()?,
        AgentConfig::new("offline"),
    ));
    let mut id = None;
    let mut trace = Vec::new();
    let mut prompts = 0;
    for (index, text) in ["first", "second", "denied"].into_iter().enumerate() {
        if index == 2 {
            cache
                .get(workspace.path(), id.as_deref().unwrap())
                .unwrap()
                .revoke_all();
        }
        let context = TurnExecutionContext::new().with_approval_sessions(cache.clone());
        let (handle, control) = RunHandle::channel(
            context,
            RunControlConfig {
                handle_approvals: true,
                ..Default::default()
            },
        )?;
        let frames = Arc::new(Mutex::new(Vec::new()));
        let captured = frames.clone();
        let agent = agent.clone();
        let repository = repository.clone();
        let selection = id
            .clone()
            .map_or(SessionSelection::New, SessionSelection::Existing);
        let worker = tokio::spawn(async move {
            agent
                .run_with_session_controlled(
                    repository.as_ref(),
                    selection,
                    text,
                    Arc::new(move |frame: &AgentRawEventEnvelope| {
                        captured.lock().unwrap().push(frame.clone())
                    }),
                    control,
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let snapshot = handle.snapshot();
                if let Some(request) = snapshot.approvals.first() {
                    assert_ne!(index, 1, "cached grant must avoid another prompt");
                    assert!(request.session.is_some());
                    assert!(request.invocation.preview.is_some());
                    prompts += 1;
                    handle
                        .respond_approval_decision(
                            &request.id,
                            if index == 0 {
                                ApprovalDecision::AllowSession
                            } else {
                                ApprovalDecision::Deny
                            },
                        )
                        .unwrap();
                }
                if matches!(
                    snapshot.status,
                    RunControlStatus::Finished | RunControlStatus::Dropped
                ) {
                    break;
                }
                handle.changed(snapshot.revision).await;
            }
        })
        .await?;
        let outcome = worker.await?;
        let AgentTurnOutcome::Completed(result) = outcome else {
            anyhow::bail!("{outcome:?}")
        };
        id = Some(result.session.unwrap().id);
        let frames = frames.lock().unwrap();
        let mut validator = events::EventTraceValidator::new();
        for frame in frames.iter() {
            validator.observe(frame)?;
            trace.push(frame.clone());
        }
        validator.finish()?;
    }
    ensure!(prompts == 2 && fs::read_to_string(workspace.path().join("file.txt"))? == "second");
    ensure!(trace.iter().any(|frame|matches!(&frame.event,AgentRawEvent::ToolApprovalResolved{resolution:Some(resolution),..} if resolution.source==ApprovalSource::SessionGrant)));
    let scope = cache.get(workspace.path(), id.as_deref().unwrap()).unwrap();
    ensure!(scope.snapshot().grants.is_empty());
    let log = fs::read_dir(storage.path())?
        .filter_map(Result::ok)
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .expect("session log");
    ensure!(!fs::read_to_string(log.path())?.contains(&scope.snapshot().session.scope_id));
    if let Some(path) = std::env::args().nth(1) {
        fs::write(
            path,
            trace
                .iter()
                .map(|frame| serde_json::to_string(frame).expect("frame"))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )?;
    }
    println!(
        "{}",
        json!({"status":"completed","turns":3,"prompts":prompts,"grant_reused":true,"revoked":true,"history_has_no_authority":true,"event_frames":trace.len()})
    );
    Ok(())
}
