//! Shared configuration and assembly with a custom model and in-memory storage, no credentials.
use aicoder_core::{model::*, session::MemorySessionRepository, *};
use aicoder_runtime::{
    ConfigDiscovery, ModelSettings, Runtime, RuntimeOverrides, Setting, Settings,
};
use async_trait::async_trait;
use std::{collections::BTreeMap, sync::Arc};
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
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "shared runtime ready"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let runtime = Runtime::load_with_environment(
        std::env::current_dir()?,
        RuntimeOverrides {
            settings: Settings {
                model: Setting::Value("offline".into()),
                options: ModelSettings {
                    max_output_tokens: Setting::Value(32),
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
        Arc::new(BTreeMap::<String, String>::new()),
    )?;
    let executor = runtime.executor_builder(Arc::new(Offline))?.build()?;
    let agent = Agent::new(executor, runtime.agent_config());
    let store = MemorySessionRepository::new();
    let result = agent
        .run_with_session(&store, SessionSelection::New, "hello", Arc::new(()))
        .await?;
    println!(
        "{}",
        serde_json::json!({"status":"completed","model":runtime.config().provider.model,"rounds":result.execution_result.rounds,"max_output_tokens":runtime.config().options.max_output_tokens,"answer":result.execution_result.final_message.text_content()})
    );
    Ok(())
}
