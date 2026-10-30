//! Explicit user-selected immutable text pages, using the same bounded filesystem backend as tools.
use aicoder_core::{
    TurnExecutionContext,
    tools::{ExecutableTool, ToolContext},
};
use anyhow::Result;
use serde_json::{Value, json};
#[derive(Debug, Clone)]
pub struct PromptReference {
    page: Value,
    encoded: String,
}
impl PromptReference {
    pub fn page(&self) -> &Value {
        &self.page
    }
    pub fn encoded(&self) -> &str {
        &self.encoded
    }
    pub fn label(&self) -> &str {
        self.page["path"].as_str().unwrap_or("file")
    }
}
impl crate::Runtime {
    /// A host/user action, not a forged model tool result. Partial pages retain continuation metadata.
    pub async fn read_reference(
        &self,
        path: &str,
        context: TurnExecutionContext,
    ) -> Result<PromptReference> {
        let tool_context = ToolContext::new(
            &self.config().workspace,
            std::time::Duration::from_secs(2),
            16 * 1024,
        )?;
        let work = crate::tools::ReadFileTool
            .execute(&tool_context, json!({"path":path,"limit_lines":4000}));
        let output = tokio::select! {biased;error=context.cancelled()=>return Err(error.into()),result=work=>result.map_err(|error|anyhow::anyhow!("{}: {}",error.code,error.message))?};
        anyhow::ensure!(
            !output.output["content"]
                .as_str()
                .is_some_and(|s| s.contains('\0')),
            "reference is binary (NUL in text page)"
        );
        let encoded = serde_json::to_string(&output.output)?;
        anyhow::ensure!(encoded.len() <= 16 * 1024, "reference page budget exceeded");
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        Ok(PromptReference {
            page: output.output,
            encoded,
        })
    }
}
