use super::*;
use crate::{
    model::ToolDefinition,
    tools::{ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess},
};
use serde_json::{Value, json};
/// Read bounded raw-byte ranges as explicitly lossy UTF-8 text for a model.
/// Hosts use ArtifactStore::read for byte-exact downloads and binary data.
pub struct ReadArtifactTool;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    id: String,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_limit")]
    max_bytes: usize,
}
fn default_limit() -> usize {
    8192
}
#[async_trait]
impl ExecutableTool for ReadArtifactTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {name:"read_artifact".into(),description:Some("Read a bounded page of a saved tool-output artifact by its opaque id. offset/next_offset are raw-byte positions; content uses utf8_lossy. complete describes source capture, not whether more pages exist. No filesystem paths are accepted.".into()),parameters:json!({"type":"object","properties":{"id":{"type":"string"},"offset":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":1,"maximum":65536}},"required":["id"],"additionalProperties":false})}
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        let args: Arguments = serde_json::from_value(arguments).map_err(|_| {
            ToolFailure::new(
                "invalid_arguments",
                "Expected artifact id, byte offset and max_bytes 1..65536",
            )
        })?;
        if args.max_bytes == 0 || args.max_bytes > MAX_ARTIFACT_PAGE_BYTES || !valid_id(&args.id) {
            return Err(ToolFailure::new(
                "invalid_arguments",
                "Invalid artifact reference or page size",
            ));
        }
        let store = context.artifact_store().ok_or_else(|| {
            ToolFailure::new(
                "artifacts_disabled",
                "Host has not installed an artifact store",
            )
        })?;
        let capacity = context.max_output_bytes().saturating_sub(400) / 6;
        if capacity == 0 {
            return Err(ToolFailure::new(
                "output_limit_too_small",
                "Artifact page metadata does not fit output budget",
            ));
        }
        let page = store
            .read(&args.id, args.offset, args.max_bytes.min(capacity))
            .await
            .map_err(|error| ToolFailure::new(error.code, "Artifact page could not be read"))?;
        let output = json!({"artifact":page.artifact,"offset":page.offset,"bytes_read":page.bytes.len(),"content":String::from_utf8_lossy(&page.bytes),"encoding":"utf8_lossy","next_offset":page.next_offset,"has_more":page.next_offset.is_some()});
        if output.to_string().len() > context.max_output_bytes() {
            return Err(ToolFailure::new(
                "output_limit_too_small",
                "Artifact page metadata does not fit output budget",
            ));
        }
        Ok(ToolSuccess {
            output,
            truncated: page.next_offset.is_some(),
            artifacts: Vec::new(),
        })
    }
}
