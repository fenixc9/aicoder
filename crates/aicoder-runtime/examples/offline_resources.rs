//! Scoped instructions, prompt expansion and progressive skill loading with a scripted model.
use aicoder_core::{
    model::*,
    session::{MemorySessionRepository, SessionEntryData, SessionRepository},
    *,
};
use aicoder_runtime::{resources::ResourceSettings, *};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
struct Offline(AtomicUsize);
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
            request
                .messages
                .iter()
                .any(|message| message.role == MessageRole::System
                    && message.text_content().contains("PROJECT_RULE"))
        );
        let step = self.0.fetch_add(1, Ordering::SeqCst);
        let message = if step == 0 {
            assert!(
                request
                    .messages
                    .last()
                    .unwrap()
                    .text_content()
                    .contains("Inspect source.rs")
            );
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "load-rust".into(),
                        name: "load_skill".into(),
                        arguments: json!({"name":"rust"}),
                    },
                }],
            }
        } else {
            assert!(
                request
                    .messages
                    .iter()
                    .filter_map(Message::tool_result)
                    .any(|(_, result, _)| result["output"]["content"]
                        .as_str()
                        .is_some_and(|content| content.contains("SKILL_RULE")))
            );
            Message::text(MessageRole::Assistant, "resource workflow ready")
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if step == 0 {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let global = tempfile::tempdir()?;
    std::fs::create_dir_all(workspace.path().join(".aicoder/skills/rust/scripts"))?;
    std::fs::create_dir_all(workspace.path().join(".aicoder/prompts"))?;
    std::fs::write(
        workspace.path().join("AGENTS.md"),
        "PROJECT_RULE: inspect before changing code.",
    )?;
    std::fs::write(
        workspace.path().join(".aicoder/skills/rust/SKILL.md"),
        "---\nname: rust\ndescription: Help inspect Rust projects\n---\nSKILL_RULE: use the project test suite.\nScripts are available under scripts/.",
    )?;
    std::fs::write(
        workspace
            .path()
            .join(".aicoder/skills/rust/scripts/check.sh"),
        "touch SHOULD_NOT_RUN",
    )?;
    std::fs::write(
        workspace.path().join(".aicoder/prompts/review.md"),
        "Inspect $1",
    )?;
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                model: Setting::Value("offline".into()),
                resources: ResourceSettings {
                    parents: Setting::Value(false),
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
            global.path().to_string_lossy().to_string(),
        )])),
    )?;
    let prompt = runtime.prepare_prompt("/review source.rs")?;
    let agent = Agent::new(
        runtime
            .executor_builder(Arc::new(Offline(AtomicUsize::new(0))))?
            .build()?,
        runtime.agent_config(),
    );
    let store = MemorySessionRepository::new();
    let result = agent
        .run_with_session(&store, SessionSelection::New, prompt.text, Arc::new(()))
        .await?;
    let session = store.open(&result.session.unwrap().id)?;
    let stored=session.entries().iter().any(|entry|matches!(&entry.data,SessionEntryData::Extension {namespace,..} if namespace=="aicoder.input_context"));
    anyhow::ensure!(
        stored && !workspace.path().join("SHOULD_NOT_RUN").exists(),
        "resource workflow failed"
    );
    println!(
        "{}",
        json!({"status":"completed","rounds":result.execution_result.rounds,"resource_files":runtime.resources()?.manifest().files.len(),"provenance_stored":stored,"answer":result.execution_result.final_message.text_content()})
    );
    Ok(())
}
