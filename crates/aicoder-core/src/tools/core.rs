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
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure>;
}
