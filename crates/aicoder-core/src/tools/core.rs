use async_trait::async_trait;
use serde_json::Value;

use crate::model::ToolDefinition;

use super::ToolContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCapability {
    ReadOnly,
    Mutating,
    Command,
}

#[derive(Debug, Clone)]
pub struct ToolSuccess {
    pub output: Value,
    pub truncated: bool,
}

impl ToolSuccess {
    pub fn new(output: Value) -> Self {
        Self {
            output,
            truncated: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolFailure {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl ToolFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

#[async_trait]
pub trait ExecutableTool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    fn capability(&self) -> ToolCapability;
    /// Read-only preparation before approval. The returned plan must retain the exact
    /// operation being reviewed; dropping it must not mutate external state.
    async fn prepare(
        &self,
        _context: &ToolContext,
        _arguments: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        Ok(None)
    }
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure>;
}

/// An immutable operation prepared before approval and consumed once after approval.
#[async_trait]
pub trait PreparedTool: Send + Sync {
    fn preview(&self) -> super::ToolPreview;
    async fn execute(self: Box<Self>, context: &ToolContext) -> Result<ToolSuccess, ToolFailure>;
}
