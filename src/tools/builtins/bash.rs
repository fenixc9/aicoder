use std::{os::unix::process::CommandExt, time::Duration};

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
        command.as_std_mut().process_group(0);
        for secret in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "DASHSCOPE_API_KEY",
            "ZHIPU_API_KEY",
        ] {
            command.env_remove(secret);
        }

        let child = command
            .spawn()
            .map_err(|error| ToolFailure::new("command_failed", error.to_string()))?;
        let mut process_group = ProcessGroupGuard::new(child.id());
        let output = timeout(command_timeout, child.wait_with_output()).await;
        let output = match output {
            Ok(Ok(output)) => {
                process_group.disarm();
                output
            }
            Ok(Err(error)) => {
                return Err(ToolFailure::new("command_failed", error.to_string()));
            }
            Err(_) => {
                return Err(ToolFailure::new(
                    "timeout",
                    format!(
                        "Command exceeded {} second timeout",
                        command_timeout.as_secs()
                    ),
                ));
            }
        };

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

struct ProcessGroupGuard {
    process_group: Option<i32>,
}

impl ProcessGroupGuard {
    fn new(child_id: Option<u32>) -> Self {
        Self {
            process_group: child_id.and_then(|id| i32::try_from(id).ok()),
        }
    }

    fn disarm(&mut self) {
        self.process_group = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(process_group) = self.process_group {
            // The shell is its own process-group leader, so a negative pid targets all descendants.
            unsafe {
                libc::kill(-process_group, libc::SIGKILL);
            }
        }
    }
}
