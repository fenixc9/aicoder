//! Archive a long shell result, read its hidden middle, and reopen the exact bytes offline.
use aicoder_core::{artifacts::*, model::*, session::*, tools::AllowAllApproval, *};
use aicoder_runtime::{
    ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings, resources::ResourceSettings,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::BTreeMap,
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
        let mut response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        let last = request.messages.last().unwrap();
        let call = if let Some((id, result, _)) = last.tool_result() {
            if id == "shell" {
                assert_eq!(result["ok"], true);
                assert_eq!(result["truncated"], true);
                assert!(
                    !result["output"]["stdout"]
                        .as_str()
                        .unwrap()
                        .contains("MIDDLE_MARKER")
                );
                let id = result["artifacts"][0]["id"].as_str().unwrap();
                Some(ModelToolCall {
                    id: "page".into(),
                    name: "read_artifact".into(),
                    arguments: json!({"id":id,"offset":100000,"max_bytes":64}),
                })
            } else {
                assert!(
                    result["output"]["content"]
                        .as_str()
                        .unwrap()
                        .contains("MIDDLE_MARKER")
                );
                None
            }
        } else {
            Some(ModelToolCall {
                id: "shell".into(),
                name: "bash".into(),
                arguments: json!({"command":"printf '%0100000d' 0; printf MIDDLE_MARKER; printf '%0100000d' 0"}),
            })
        };
        if let Some(call) = call {
            response.message = Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall { call }],
            };
            response.stop_reason = StopReason::ToolCalls;
        }
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::from([(
            "AICODER_HOME".into(),
            state.path().to_string_lossy().into_owned(),
        )])),
    )?;
    let repo = runtime.sessions()?;
    let executor = runtime
        .executor_builder(Arc::new(Offline))?
        .approval(AllowAllApproval)
        .build()?;
    let agent = Agent::new(executor, runtime.agent_config());
    let frames = Arc::new(Mutex::new(Vec::new()));
    let captured = frames.clone();
    let result = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::New,
            "inspect long output",
            Arc::new(move |frame: &AgentRawEventEnvelope| {
                captured.lock().unwrap().push(frame.clone())
            }),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    let session = repo.open(&result.session.unwrap().id)?;
    let model = session.selected_branch().model_context()?.into_messages();
    let reference = model
        .iter()
        .find_map(|m| {
            m.tool_result()
                .filter(|(id, _, _)| *id == "shell")
                .map(|(_, value, _)| {
                    serde_json::from_value::<ArtifactRef>(value["artifacts"][0].clone()).unwrap()
                })
        })
        .unwrap();
    let store = runtime.artifact_store_for(workspace.path())?.unwrap();
    let mut bytes = Vec::new();
    let mut offset = 0;
    loop {
        let page = store.read(&reference.id, offset, 65536).await?;
        bytes.extend(page.bytes);
        if let Some(next) = page.next_offset {
            offset = next;
        } else {
            break;
        }
    }
    let mut expected = vec![b'0'; 100000];
    expected.extend(b"MIDDLE_MARKER");
    expected.extend(vec![b'0'; 100000]);
    ensure!(bytes == expected && reference.complete);
    let frames = frames.lock().unwrap();
    let mut validator = events::EventTraceValidator::new();
    for frame in frames.iter() {
        validator.observe(frame)?;
    }
    validator.finish()?;
    ensure!(frames.iter().any(|frame|matches!(&frame.event,AgentRawEvent::ToolExecutionEnded {artifacts,..} if artifacts==std::slice::from_ref(&reference))));
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
        json!({"status":"completed","artifact_bytes":bytes.len(),"complete":reference.complete,"preview_truncated":true,"middle_recovered_by_tool":true,"reopened_bytes_match":true,"coding_rounds":result.execution_result.rounds,"event_frames":frames.len()})
    );
    Ok(())
}
