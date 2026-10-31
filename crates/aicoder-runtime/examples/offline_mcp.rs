//! Scripted Agent over MCP stdio. Optional argv selects another trusted server for interop.
use aicoder_core::{extensions::ExtensionRegistry, model::*, tools::*, *};
use aicoder_runtime::{mcp::*, process_extensions::ProcessConfig};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
struct Provider {
    tool: String,
    arguments: Value,
}
#[async_trait]
impl ModelProvider for Provider {
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
            message: Message::text(MessageRole::Assistant, "MCP request completed"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        if request.messages.last().unwrap().role == MessageRole::User {
            response.stop_reason = StopReason::ToolCalls;
            response.message.content = vec![ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "mcp-call".into(),
                    name: self.tool.clone(),
                    arguments: self.arguments.clone(),
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
    let note = root.path().join("note.txt");
    std::fs::write(&note, "MCP offline fixture")?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let fixture = args.is_empty();
    let program = args
        .first()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("aicoder-mcp-fixture")
        });
    let mut process = ProcessConfig::new(program, root.path(), "mcp");
    process.arguments = if fixture {
        vec!["modern".into()]
    } else {
        args.iter()
            .skip(1)
            .map(|arg| {
                arg.to_string_lossy()
                    .replace("{workspace}", &root.path().to_string_lossy())
            })
            .collect()
    };
    let mcp = McpExtension::connect(McpConfig::new(process), TurnExecutionContext::new()).await?;
    let requested = if fixture {
        "write"
    } else if mcp.catalog().iter().any(|t| t.name == "read_text_file") {
        "read_text_file"
    } else {
        "read_file"
    };
    let tool = mcp
        .catalog()
        .iter()
        .find(|tool| tool.name == requested)
        .ok_or_else(|| anyhow::anyhow!("example requires a file read/write tool"))?;
    let model_name = tool.model_name.clone();
    let arguments = if fixture {
        json!({"path":"note.txt","content":"MCP approved write"})
    } else {
        json!({"path":note})
    };
    let mut registry = ExtensionRegistry::new();
    registry.install(mcp.extension()?)?;
    let agent = Agent::new(
        TurnExecutor::builder(Provider {
            tool: model_name.clone(),
            arguments,
        })
        .workspace(root.path())
        .extensions(Arc::new(registry.finish()))
        .approval(AllowAllApproval)
        .build()?,
        AgentConfig::new("scripted"),
    );
    let result = agent
        .run("Run the scoped MCP fixture", Arc::new(()))
        .await?;
    let body = result
        .execution_result
        .messages
        .iter()
        .find_map(|message| message.tool_result().map(|(_, body, _)| body))
        .ok_or_else(|| anyhow::anyhow!("missing MCP tool result"))?;
    anyhow::ensure!(body["ok"] == true, "MCP tool failed: {body}");
    if fixture {
        anyhow::ensure!(
            std::fs::read_to_string(&note)? == "MCP approved write",
            "MCP write did not commit"
        );
    } else {
        anyhow::ensure!(
            body.to_string().contains("MCP offline fixture"),
            "MCP read did not return the fixture"
        );
    }
    let diagnostics = mcp.shutdown().await;
    anyhow::ensure!(diagnostics.reaped, "MCP process cleanup incomplete");
    println!(
        "{}",
        json!({"protocol":mcp.protocol_version(),"server":mcp.server(),"tools":mcp.catalog().len(),"selected_tool":model_name,"rounds":result.execution_result.rounds,"wire":mcp.wire_diagnostics(),"diagnostics":diagnostics,"model_network_requests":0})
    );
    Ok(())
}
