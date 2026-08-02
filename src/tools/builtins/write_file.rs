use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;
use uuid::Uuid;

use crate::types::FunctionDefinition;

use super::super::{
    ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess, util::parse_arguments,
};

pub struct WriteFileTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteFileArguments {
    path: String,
    content: String,
}

#[async_trait]
impl ExecutableTool for WriteFileTool {
    fn definition(&self) -> FunctionDefinition {
        FunctionDefinition {
            name: "write_file".to_string(),
            description: Some(
                "Create or replace a UTF-8 file inside the workspace. Parent must exist."
                    .to_string(),
            ),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            })),
        }
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }

    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        let arguments: WriteFileArguments = parse_arguments(arguments)?;
        let path = context.resolve_for_write(&arguments.path)?;
        if path.exists() && !path.is_file() {
            return Err(ToolFailure::new("not_a_file", "Path is not a file"));
        }

        let parent = path.parent().ok_or_else(|| {
            ToolFailure::new("invalid_path", format!("Invalid path: {}", path.display()))
        })?;
        let temporary = parent.join(format!(".aicoder-{}.tmp", Uuid::new_v4()));
        fs::write(&temporary, arguments.content.as_bytes())
            .await
            .map_err(|error| ToolFailure::new("write_failed", error.to_string()))?;

        if let Ok(metadata) = fs::metadata(&path).await
            && let Err(error) = fs::set_permissions(&temporary, metadata.permissions()).await
        {
            let _ = fs::remove_file(&temporary).await;
            return Err(ToolFailure::new("write_failed", error.to_string()));
        }
        if let Err(error) = fs::rename(&temporary, &path).await {
            let _ = fs::remove_file(&temporary).await;
            return Err(ToolFailure::new("write_failed", error.to_string()));
        }

        Ok(ToolSuccess::new(json!({
            "path": context.relative_path(&path),
            "bytes_written": arguments.content.len(),
        })))
    }
}
