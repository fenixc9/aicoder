//! Host a separately built Rust fixture through the same core loop and approval contract.
use aicoder_core::{extensions::ExtensionRegistry, model::*, tools::*, *};
use aicoder_runtime::process_extensions::*;
use async_trait::async_trait;
use serde_json::json;
use std::{sync::Arc, time::Duration};
struct Provider;
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("offline", model);
        s.capabilities.tools = true;
        Ok(s)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let last = request.messages.last().unwrap();
        let mut response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "external tool completed"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        if last.role == MessageRole::User {
            response.stop_reason = StopReason::ToolCalls;
            response.message.content = vec![ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "external-write".into(),
                    name: "fixture__write".into(),
                    arguments: json!({"path":"note.txt","content":"after"}),
                },
            }];
        }
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("note.txt"), "before")?;
    let program = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("aicoder-process-fixture")
        });
    let process = ProcessExtension::connect(
        ProcessConfig::new(program, root.path(), "fixture"),
        TurnExecutionContext::new(),
    )
    .await?;
    let mut registry = ExtensionRegistry::new();
    registry.install(process.extension()?)?;
    let extension = Arc::new(registry.finish());
    let agent = Agent::new(
        TurnExecutor::builder(Provider)
            .workspace(root.path())
            .extensions(extension.clone())
            .approval(AllowAllApproval)
            .build()?,
        AgentConfig::new("offline"),
    );
    let result = agent.run("perform the captured edit", Arc::new(())).await?;
    anyhow::ensure!(
        std::fs::read_to_string(root.path().join("note.txt"))? == "after",
        "external plan was not committed"
    );
    let context = ToolContext::new(root.path(), Duration::from_secs(2), 65536)?;
    let tool = process
        .tools()
        .into_iter()
        .find(|t| t.definition().name == "fixture__write")
        .unwrap();
    drop(
        tool.prepare(
            &context,
            json!({"path":"note.txt","content":"must be discarded"}),
        )
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?,
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while process.diagnostics().completed_requests < 5 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await?;
    anyhow::ensure!(
        std::fs::read_to_string(root.path().join("note.txt"))? == "after",
        "discarded plan changed the workspace"
    );
    let diagnostics = process.shutdown().await;
    anyhow::ensure!(diagnostics.reaped, "process cleanup incomplete");
    println!(
        "{}",
        json!({"server":process.server(),"catalog":extension.catalog(),"rounds":result.execution_result.rounds,"diagnostics":diagnostics,"network_requests":0})
    );
    Ok(())
}
