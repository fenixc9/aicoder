use std::io::{self, Write};

use anyhow::{Context, Result};
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

pub struct ConsoleApproval;

#[async_trait]
impl ApprovalHandler for ConsoleApproval {
    async fn approve(&self, invocation: &ToolInvocation) -> Result<bool> {
        let invocation = invocation.clone();
        tokio::task::spawn_blocking(move || {
            eprintln!(
                "\n⚠️  工具 {} 将以当前用户权限执行（不是沙箱）\n参数: {}",
                invocation.name,
                serde_json::to_string_pretty(&invocation.arguments)?
            );
            eprint!("允许执行? [y/N] ");
            io::stderr().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            Ok(matches!(
                input.trim().to_ascii_lowercase().as_str(),
                "y" | "yes"
            ))
        })
        .await
        .context("Approval prompt task failed")?
    }
}
