use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{process::Command, time::timeout};

use crate::types::FunctionDefinition;

use super::super::{
    ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess,
    util::{parse_arguments, truncate_utf8},
};

pub struct BashTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashArguments {
    command: String,
    timeout_seconds: Option<u64>,
}

#[async_trait]
impl ExecutableTool for BashTool {
    fn definition(&self) -> FunctionDefinition {
        FunctionDefinition {
            name: "bash".to_string(),
            description: Some(
                "Run a non-interactive bash command from the workspace. This is not sandboxed."
                    .to_string(),
            ),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 30}
                },
                "required": ["command"],
                "additionalProperties": false
            })),
        }
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::Command
    }

    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        let arguments: BashArguments = parse_arguments(arguments)?;
        if let Some(timeout_seconds) = arguments.timeout_seconds
            && !(1..=30).contains(&timeout_seconds)
        {
            return Err(ToolFailure::new(
                "invalid_arguments",
                "timeout_seconds must be between 1 and 30",
            ));
        }
        let requested_timeout = Duration::from_secs(arguments.timeout_seconds.unwrap_or(30));
        let command_timeout = requested_timeout.min(context.tool_timeout());

        let mut command = Command::new("/bin/bash");
        command
            .arg("-lc")
            .arg(&arguments.command)
            .current_dir(context.workspace_root())
            .kill_on_drop(true);
        for secret in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "DASHSCOPE_API_KEY",
            "ZHIPU_API_KEY",
        ] {
            command.env_remove(secret);
        }

        let output = timeout(command_timeout, command.output())
            .await
            .map_err(|_| {
                ToolFailure::new(
                    "timeout",
                    format!(
                        "Command exceeded {} second timeout",
                        command_timeout.as_secs()
                    ),
                )
            })?
            .map_err(|error| ToolFailure::new("command_failed", error.to_string()))?;

        let per_stream_limit = (context.max_output_bytes() / 2).max(1);
        let (stdout, stdout_truncated) =
            truncate_utf8(&String::from_utf8_lossy(&output.stdout), per_stream_limit);
        let (stderr, stderr_truncated) =
            truncate_utf8(&String::from_utf8_lossy(&output.stderr), per_stream_limit);
        let details = json!({
            "exit_code": output.status.code(),
            "stdout": stdout,
            "stderr": stderr,
            "truncated": stdout_truncated || stderr_truncated,
        });

        if !output.status.success() {
            return Err(ToolFailure::new(
                "command_failed",
                format!("Command exited with status {}", output.status),
            )
            .with_details(details));
        }

        Ok(ToolSuccess {
            output: details,
            truncated: stdout_truncated || stderr_truncated,
        })
    }
}
