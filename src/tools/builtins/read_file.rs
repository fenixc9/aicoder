use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader},
};

use crate::types::FunctionDefinition;

use super::super::{
    ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess,
    util::{parse_arguments, truncate_utf8},
};

pub struct ReadFileTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileArguments {
    path: String,
    #[serde(default = "default_offset_line")]
    offset_line: usize,
    #[serde(default = "default_limit_lines")]
    limit_lines: usize,
}

fn default_offset_line() -> usize {
    1
}

fn default_limit_lines() -> usize {
    400
}

#[async_trait]
impl ExecutableTool for ReadFileTool {
    fn definition(&self) -> FunctionDefinition {
        FunctionDefinition {
            name: "read_file".to_string(),
            description: Some(
                "Read a bounded page of UTF-8 text inside the workspace. Continue from next_offset_line while has_more is true."
                    .to_string(),
            ),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "offset_line": {"type": "integer", "minimum": 1, "default": 1},
                    "limit_lines": {"type": "integer", "minimum": 1, "maximum": 4000, "default": 400}
                },
                "required": ["path"],
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
        let arguments: ReadFileArguments = parse_arguments(arguments)?;
        if arguments.offset_line == 0 || !(1..=4000).contains(&arguments.limit_lines) {
            return Err(ToolFailure::new(
                "invalid_arguments",
                "offset_line must be >= 1 and limit_lines must be between 1 and 4000",
            ));
        }
        let path = context.resolve_existing(&arguments.path)?;
        if !path.is_file() {
            return Err(ToolFailure::new("not_a_file", "Path is not a file"));
        }
        let file = File::open(&path)
            .await
            .map_err(|error| ToolFailure::new("read_failed", error.to_string()))?;
        let mut lines = BufReader::new(file).lines();

        for _ in 1..arguments.offset_line {
            if next_line(&mut lines).await?.is_none() {
                return Ok(ToolSuccess::new(json!({
                    "path": context.relative_path(&path),
                    "offset_line": arguments.offset_line,
                    "lines_read": 0,
                    "end_line": null,
                    "content": "",
                    "content_truncated": false,
                    "has_more": false,
                    "next_offset_line": null,
                })));
            }
        }

        let max_bytes = context.max_output_bytes();
        let mut content = String::new();
        let mut lines_read = 0_usize;
        let mut reached_eof = false;
        let mut content_truncated = false;

        while lines_read < arguments.limit_lines {
            let Some(line) = next_line(&mut lines).await? else {
                reached_eof = true;
                break;
            };

            let separator_bytes = usize::from(lines_read > 0);
            if content.len() + separator_bytes + line.len() > max_bytes {
                if separator_bytes == 1 && content.len() < max_bytes {
                    content.push('\n');
                }
                let remaining = max_bytes.saturating_sub(content.len());
                let (partial, _) = truncate_utf8(&line, remaining);
                content.push_str(&partial);
                lines_read += 1;
                content_truncated = true;
                break;
            }

            if separator_bytes == 1 {
                content.push('\n');
            }
            content.push_str(&line);
            lines_read += 1;
        }

        let has_more = if content_truncated {
            true
        } else if reached_eof {
            false
        } else {
            next_line(&mut lines).await?.is_some()
        };
        let end_line =
            (lines_read > 0).then(|| arguments.offset_line.saturating_add(lines_read - 1));
        let next_offset_line =
            has_more.then(|| end_line.unwrap_or(arguments.offset_line).saturating_add(1));

        Ok(ToolSuccess {
            output: json!({
                "path": context.relative_path(&path),
                "offset_line": arguments.offset_line,
                "lines_read": lines_read,
                "end_line": end_line,
                "content": content,
                "content_truncated": content_truncated,
                "has_more": has_more,
                "next_offset_line": next_offset_line,
            }),
            truncated: has_more || content_truncated,
        })
    }
}

async fn next_line(
    lines: &mut tokio::io::Lines<BufReader<File>>,
) -> Result<Option<String>, ToolFailure> {
    lines
        .next_line()
        .await
        .map_err(|error| ToolFailure::new("read_failed", error.to_string()))
}
