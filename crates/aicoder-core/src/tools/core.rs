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
    pub artifacts: Vec<crate::artifacts::ArtifactRef>,
}

impl ToolSuccess {
    pub fn new(output: Value) -> Self {
        Self {
            output,
            truncated: false,
            artifacts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolFailure {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
    pub artifacts: Vec<crate::artifacts::ArtifactRef>,
}

impl ToolFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            artifacts: Vec::new(),
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

#[async_trait]
pub trait ExecutableTool: Send + Sync {
    /// Host declaration for independent operations; this never changes approval requirements.
    fn parallel_safe(&self) -> bool {
        self.capability() == ToolCapability::ReadOnly
    }
    fn composition_factory(&self) -> Option<&dyn crate::composition::ToolCompositionFactory> {
        None
    }
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

/// Host-declared effects of an owned operation, not an authorization or OS sandbox proof.
/// Mutation guards should fail closed for Unknown and retain the same plan after checking.
#[derive(Debug, Clone, Copy)]
pub enum PreparedToolEffects<'a> {
    Unknown,
    FileMutation { paths: &'a [std::path::PathBuf] },
}
/// An immutable operation prepared before approval and consumed once after approval.
#[async_trait]
pub trait PreparedTool: Send + Sync {
    /// Optional non-chat model operation, invoked only by a metered Agent dispatcher.
    fn auxiliary_operation(&self) -> Option<&dyn crate::auxiliary::AuxiliaryOperation> {
        None
    }
    fn effects(&self) -> PreparedToolEffects<'_> {
        PreparedToolEffects::Unknown
    }
    fn preview(&self) -> super::ToolPreview;
    async fn execute(self: Box<Self>, context: &ToolContext) -> Result<ToolSuccess, ToolFailure>;
}
