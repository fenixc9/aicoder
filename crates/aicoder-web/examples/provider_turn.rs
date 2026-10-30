//! A real SDK/runtime invocation for the offline cross-frontend HTTP fixture driver.
#![cfg(feature = "server")]
use aicoder_core::{Agent, TurnExecutionContext};
use aicoder_runtime::{
    ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings, environment_file,
    resources::ResourceSettings,
};
use std::sync::Arc;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let workspace = std::env::args_os()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("Pass the temporary fixture workspace"))?;
    let runtime = Runtime::load_with_environment(
        workspace,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Clear,
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        environment_file(None)?,
    )?;
    let agent = Agent::new(
        runtime
            .executor_builder(Arc::new(runtime.session_client()?))?
            .build()?,
        runtime.agent_config(),
    );
    let result = agent
        .run_outcome("parity", Arc::new(()), TurnExecutionContext::new())
        .await
        .into_result()?;
    println!(
        "{}",
        serde_json::json!({"text":result.execution_result.final_message.text_content(),"rounds":result.execution_result.progress.rounds,"usage":result.execution_result.progress.usage})
    );
    Ok(())
}
