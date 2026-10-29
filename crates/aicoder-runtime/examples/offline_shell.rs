//! Real bounded shell capture, timeout and model recovery using a scripted provider.
use aicoder_core::tools::{ExecutableTool, ToolContext};
use aicoder_core::{
    model::*,
    tools::{AllowAllApproval, DispatcherConfig},
    *,
};
use aicoder_runtime::tools::{BashTool, ShellLimits, ShellOutput, default_registry};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
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
        let step = self.0.fetch_add(1, Ordering::SeqCst);
        let message = match step {
            0 => call(
                "logs",
                "printf HEAD; for ((i=0;i<32;i++)); do printf '%4096s' X; done; printf TAIL",
                None,
            ),
            1 => {
                let output = request.messages.last().unwrap().tool_result().unwrap().1;
                assert_eq!(output["ok"], true);
                assert_eq!(output["output"]["reaped"], true);
                assert_eq!(output["output"]["output_complete"], true);
                assert_eq!(output["output"]["truncated"], true);
                assert!(
                    output["output"]["stdout"]
                        .as_str()
                        .unwrap()
                        .ends_with("TAIL")
                );
                call("deadline", "printf partial; sleep 30", Some(1))
            }
            _ => {
                let output = request.messages.last().unwrap().tool_result().unwrap().1;
                assert_eq!(output["error"]["code"], "timeout");
                assert_eq!(output["error"]["details"]["stdout"], "partial");
                assert_eq!(output["error"]["details"]["reaped"], true);
                Message::text(MessageRole::Assistant, "bounded logs and timeout handled")
            }
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if step < 2 {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage::default(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
fn call(id: &str, command: &str, timeout: Option<u64>) -> Message {
    let mut arguments = json!({"command":command});
    if let Some(timeout) = timeout {
        arguments["timeout_seconds"] = json!(timeout);
    }
    Message {
        role: MessageRole::Assistant,
        content: vec![ContentBlock::ToolCall {
            call: ModelToolCall {
                id: id.into(),
                name: "bash".into(),
                arguments,
            },
        }],
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() == 2 && args[0] == "--capture-bytes" {
        let count: u64 = args[1].parse()?;
        return capture_sample(count).await;
    }
    ensure!(args.is_empty(), "usage: offline_shell [--capture-bytes N]");
    let workspace = tempfile::tempdir()?;
    let executor = TurnExecutor::builder(Offline(AtomicUsize::new(0)))
        .workspace(workspace.path())
        .registry(default_registry()?)
        .approval(AllowAllApproval)
        .dispatcher_config(DispatcherConfig {
            max_output_bytes: 4096,
            ..Default::default()
        })
        .build()?;
    let result = executor
        .run(ModelRequest::new(
            "offline",
            vec![Message::text(MessageRole::User, "test shell contract")],
        ))
        .await?;
    ensure!(result.rounds == 3 && result.tool_calls == 2);
    println!(
        "{}",
        json!({"status":"completed","rounds":result.rounds,"tool_calls":result.tool_calls,"answer":result.final_message.text_content(),"checks":["bounded_prefix_tail","exit_and_reap","partial_timeout","model_recovery"]})
    );
    Ok(())
}

async fn capture_sample(count: u64) -> Result<()> {
    ensure!(
        (8..=64 * 1024 * 1024).contains(&count),
        "N must be 8..64 MiB"
    );
    let workspace = tempfile::tempdir()?;
    let body = count - 8;
    let command = format!(
        "printf HEAD; for ((i=0;i<{};i++)); do printf '%4096s' X; done; {} printf TAIL",
        body / 4096,
        if body.is_multiple_of(4096) {
            String::new()
        } else {
            format!("printf '%{}s' X;", body % 4096)
        }
    );
    let context = ToolContext::new(workspace.path(), std::time::Duration::from_secs(10), 4096)?;
    let limits = ShellLimits {
        max_output_bytes: count,
        ..Default::default()
    };
    let result = BashTool::with_limits(limits)?
        .execute(&context, json!({"command":command}))
        .await
        .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?;
    let serialized_bytes = result.output.to_string().len();
    let output: ShellOutput = serde_json::from_value(result.output)?;
    ensure!(output.stdout_bytes == count && output.output_complete && output.reaped);
    ensure!(
        serialized_bytes <= 4096
            && output.stdout.starts_with("HEAD")
            && output.stdout.ends_with("TAIL")
    );
    println!(
        "{}",
        json!({"status":"completed","observed_bytes":output.stdout_bytes,"omitted_bytes":output.stdout_omitted_bytes,"serialized_bytes":serialized_bytes,"limits":limits,"reaped":output.reaped})
    );
    Ok(())
}
