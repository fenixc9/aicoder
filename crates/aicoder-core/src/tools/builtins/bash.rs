use std::{os::unix::process::CommandExt, process::Stdio, time::Duration};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{process::Command, time::timeout};

use crate::model::ToolDefinition;

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
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: Some(
                "Run a non-interactive bash command from the workspace. This is not sandboxed."
                    .to_string(),
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 30}
                },
                "required": ["command"],
                "additionalProperties": false
            }),
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
        execute_bash(context, arguments, &[]).await
    }
}
impl BashTool {
    /// Add host-owned credential references to the standard inherited-environment exclusions.
    pub fn excluding_environment(variables: Vec<String>) -> impl ExecutableTool {
        ConfiguredBash { variables }
    }
}
struct ConfiguredBash {
    variables: Vec<String>,
}
#[async_trait]
impl ExecutableTool for ConfiguredBash {
    fn definition(&self) -> ToolDefinition {
        BashTool.definition()
    }
    fn capability(&self) -> ToolCapability {
        BashTool.capability()
    }
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        execute_bash(context, arguments, &self.variables).await
    }
}
async fn execute_bash(
    context: &ToolContext,
    arguments: Value,
    excluded: &[String],
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
        .arg("-c")
        .arg(&arguments.command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(context.workspace_root())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    scrub_credentials(
        &mut command,
        std::env::var("AICODER_API_KEY_ENV").ok().as_deref(),
    );

    for variable in excluded {
        command.env_remove(variable);
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

fn scrub_credentials(command: &mut Command, referenced: Option<&str>) {
    for name in [
        "OPENAI_API_KEY",
        "DEEPSEEK_API_KEY",
        "AICODER_API_KEY",
        "AICODER_API_KEY_ENV",
        "AICODER_PROXY",
        "ANTHROPIC_API_KEY",
        "DASHSCOPE_API_KEY",
        "ZHIPU_API_KEY",
        "BASH_ENV",
    ] {
        command.env_remove(name);
    }
    if let Some(name) = referenced
        && !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn command_output_is_captured_and_stdin_is_closed() {
        let workspace = tempfile::tempdir().unwrap();
        let context = ToolContext::new(workspace.path(), Duration::from_secs(2), 4096).unwrap();
        let result=BashTool.execute(&context,json!({"command":"printf 'captured stdout'; printf 'captured stderr' >&2; if read -r line; then exit 9; fi"})).await.unwrap();
        assert_eq!(result.output["stdout"], "captured stdout");
        assert_eq!(result.output["stderr"], "captured stderr");
        assert_eq!(result.output["exit_code"], 0);
    }
    #[test]
    fn managed_and_referenced_credentials_are_removed_from_the_child_environment() {
        let mut command = Command::new("/bin/bash");
        command
            .env("AICODER_API_KEY", "fake-secret")
            .env("SERVICE_TOKEN", "fake-secret");
        scrub_credentials(&mut command, Some("SERVICE_TOKEN"));
        for name in [
            "OPENAI_API_KEY",
            "DEEPSEEK_API_KEY",
            "AICODER_API_KEY",
            "SERVICE_TOKEN",
            "BASH_ENV",
        ] {
            assert!(
                command
                    .as_std()
                    .get_envs()
                    .any(|(key, value)| key == name && value.is_none())
            );
        }
    }
}
