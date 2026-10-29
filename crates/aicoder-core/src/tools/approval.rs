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
    /// Legacy one-shot adapter; typed handlers can override `decide` instead.
    async fn approve(&self, _invocation: &ToolInvocation) -> Result<bool> {
        Ok(false)
    }
    async fn decide(&self, request: &ApprovalRequest<'_>) -> Result<ApprovalDecision> {
        self.approve(request.invocation)
            .await
            .map(ApprovalDecision::from)
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Deny,
    AllowOnce,
    AllowSession,
}
impl ApprovalDecision {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowSession)
    }
}
impl From<bool> for ApprovalDecision {
    fn from(allowed: bool) -> Self {
        if allowed { Self::AllowOnce } else { Self::Deny }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSource {
    Handler,
    RunHandle,
    SessionGrant,
}
#[derive(Debug, serde::Serialize)]
pub struct ApprovalRequest<'a> {
    pub invocation: &'a ToolInvocation,
    pub session: Option<super::ApprovalSessionInfo>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApprovalResolution {
    pub decision: ApprovalDecision,
    pub source: ApprovalSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<super::ApprovalSessionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
}
