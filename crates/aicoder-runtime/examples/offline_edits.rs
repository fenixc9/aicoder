//! Controlled diff review and conflict handling without a model service.
use aicoder_core::{model::*, *};
use aicoder_runtime::tools::default_registry;
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
        let tool_result = request.messages.iter().find_map(Message::tool_result);
        let message = if tool_result.is_some() {
            Message::text(MessageRole::Assistant, "reviewed change resolved")
        } else {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "edit".into(),
                        name: "edit_file".into(),
                        arguments: json!({"path":"file.txt","edits":[{"old_text":"before","new_text":"after"}]}),
                    },
                }],
            }
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if tool_result.is_some() {
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
    let mut trace = Vec::new();
    for conflict in [false, true] {
        let workspace = tempfile::tempdir()?;
        let path = workspace.path().join("file.txt");
        fs::write(&path, "before\n")?;
        let executor = TurnExecutor::builder(Offline)
            .workspace(workspace.path())
            .registry(default_registry()?)
            .build()?;
        let (handle, control) = RunHandle::channel(
            TurnExecutionContext::new(),
            RunControlConfig {
                handle_approvals: true,
                ..Default::default()
            },
        )?;
        let frames = Arc::new(Mutex::new(Vec::new()));
        let captured = frames.clone();
        let worker = tokio::spawn(async move {
            executor
                .run_controlled(
                    ModelRequest::new(
                        "offline",
                        vec![Message::text(MessageRole::User, "edit file")],
                    ),
                    Arc::new(move |event: &AgentRawEventEnvelope| {
                        captured.lock().unwrap().push(event.clone())
                    }),
                    control,
                )
                .await
        });
        let approval = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let snapshot = handle.snapshot();
                if let Some(approval) = snapshot.approvals.first() {
                    break approval.clone();
                }
                handle.changed(snapshot.revision).await;
            }
        })
        .await?;
        let preview = approval
            .invocation
            .preview
            .as_ref()
            .expect("prepared preview");
        ensure!(preview.body.contains("-before") && preview.body.contains("+after"));
        ensure!(fs::read_to_string(&path)? == "before\n");
        if conflict {
            fs::write(&path, "external editor\n")?;
        }
        handle.respond_approval(&approval.id, true)?;
        let outcome = worker.await?;
        let RunOutcome::Completed(result) = outcome else {
            anyhow::bail!("{outcome:?}")
        };
        let output = result
            .messages
            .iter()
            .find_map(Message::tool_result)
            .expect("tool result")
            .1;
        ensure!(output["ok"] == !conflict);
        if conflict {
            ensure!(output["error"]["code"] == "edit_conflict");
        }
        ensure!(
            fs::read_to_string(&path)?
                == if conflict {
                    "external editor\n"
                } else {
                    "after\n"
                }
        );
        let frames = frames.lock().unwrap();
        let mut validator = events::EventTraceValidator::new();
        for frame in frames.iter() {
            validator.observe(frame)?;
            trace.push(frame.clone());
        }
        validator.finish()?;
    }
    if let Some(path) = std::env::args().nth(1) {
        let bytes = trace
            .iter()
            .map(|frame| serde_json::to_string(frame).expect("frame"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(path, bytes)?;
    }
    println!(
        "{}",
        json!({"status":"completed","approved_diff":true,"external_conflict_preserved":true,"event_frames":trace.len(),"event_schema_version":events::EVENT_SCHEMA_VERSION})
    );
    Ok(())
}
