use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::ToolCapability;

#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    pub capability: ToolCapability,
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
