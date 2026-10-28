use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::types::FunctionDefinition;

use super::super::{
    ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess,
    util::{parse_arguments, truncate_utf8},
};

pub struct GrepTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepArguments {
    pattern: String,
    #[serde(default = "default_search_path")]
    path: String,
    glob: Option<String>,
    #[serde(default = "default_max_results")]
    max_results: usize,
}

fn default_search_path() -> String {
    ".".to_string()
}

fn default_max_results() -> usize {
    100
}

#[async_trait]
impl ExecutableTool for GrepTool {
    fn definition(&self) -> FunctionDefinition {
        FunctionDefinition {
            name: "grep".to_string(),
            description: Some(
                "Search workspace text using ripgrep regular expressions".to_string(),
            ),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "default": "."},
                    "glob": {"type": "string"},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 100}
                },
                "required": ["pattern"],
                "additionalProperties": false
            })),
        }
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }

    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        let arguments: GrepArguments = parse_arguments(arguments)?;
        if !(1..=1000).contains(&arguments.max_results) {
            return Err(ToolFailure::new(
                "invalid_arguments",
                "max_results must be between 1 and 1000",
            ));
        }
        let path = context.resolve_existing(&arguments.path)?;

        let mut command = Command::new("rg");
        command
            .arg("--line-number")
            .arg("--no-heading")
            .arg("--color=never");
        if let Some(glob) = &arguments.glob {
            command.arg("--glob").arg(glob);
        }
        command.arg("--").arg(&arguments.pattern).arg(&path);
        command
            .current_dir(context.workspace_root())
            .kill_on_drop(true);

        let output = command
            .output()
            .await
            .map_err(|error| ToolFailure::new("grep_failed", error.to_string()))?;
        let code = output.status.code().unwrap_or(-1);
        if code != 0 && code != 1 {
            return Err(ToolFailure::new(
                "grep_failed",
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let matches = stdout
            .lines()
            .take(arguments.max_results)
            .collect::<Vec<_>>()
            .join("\n");
        let result_limit_hit = stdout.lines().count() > arguments.max_results;
        let (matches, byte_limit_hit) = truncate_utf8(&matches, context.max_output_bytes());
        Ok(ToolSuccess {
            output: json!({
                "path": context.relative_path(&path),
                "matches": matches,
            }),
            truncated: result_limit_hit || byte_limit_hit,
        })
    }
}
