use super::{
    ExecutableTool, PreparedTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess,
    atomic_file::{FilePlan, MAX_FILE_BYTES, Target},
    util::parse_arguments,
};
use aicoder_core::model::ToolDefinition;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
pub struct WriteFileTool;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    content: String,
    expected_revision: Option<String>,
}
#[async_trait]
impl ExecutableTool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition{name:"write_file".into(),description:Some("Prepare and atomically create/replace a UTF-8 file. Approval shows the prepared diff. Use expected_revision from read_file for stale-read protection, or 'missing' to create only. Parent must exist.".into()),
            parameters:json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"expected_revision":{"type":"string"}},"required":["path","content"],"additionalProperties":false})}
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn prepare(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        let args: Arguments = parse_arguments(arguments)?;
        if args.content.len() > MAX_FILE_BYTES {
            return Err(ToolFailure::new(
                "file_too_large",
                "Replacement exceeds the 4 MiB mutation limit",
            ));
        }
        let context = context.clone();
        let plan = tokio::task::spawn_blocking(move || {
            let target = Target::load(&context, &args.path)?;
            target.expect(args.expected_revision.as_deref())?;
            let output = json!({"bytes_written":args.content.len()});
            FilePlan::new(&context, target, args.content.into_bytes(), output)
        })
        .await
        .map_err(|error| ToolFailure::new("preparation_failed", error.to_string()))??;
        Ok(Some(Box::new(plan)))
    }
    async fn execute(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        tokio::time::timeout(context.tool_timeout(), async {
            self.prepare(context, args)
                .await?
                .expect("file operation plan")
                .execute(context)
                .await
        })
        .await
        .map_err(|_| ToolFailure::new("timeout", "File write exceeded its time limit"))?
    }
}
