use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::ToolCapability;

pub const MAX_TOOL_PREVIEW_BYTES: usize = 32 * 1024;

/// Host-neutral, bounded display data. The execution plan itself is never serialized.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolPreview {
    pub summary: String,
    pub body: String,
    /// For example `unified_diff` or `text`; hosts may support additional formats.
    pub format: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolInvocation {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    pub capability: ToolCapability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<ToolPreview>,
}

#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn approve(&self, invocation: &ToolInvocation) -> Result<bool>;
}

pub struct AllowAllApproval;

#[async_trait]
impl ApprovalHandler for AllowAllApproval {
    async fn approve(&self, _invocation: &ToolInvocation) -> Result<bool> {
        Ok(true)
    }
}

/// Safe default for non-interactive library consumers.
pub struct DenyAllApproval;

#[async_trait]
impl ApprovalHandler for DenyAllApproval {
    async fn approve(&self, _invocation: &ToolInvocation) -> Result<bool> {
        Ok(false)
    }
}
