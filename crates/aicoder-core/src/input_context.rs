//! Host-owned initial system context. It cannot rewrite recorded user/tool history.
use crate::model::{Message, MessageRole};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;
pub struct PreparedInput {
    pub system_prefix: Vec<Message>,
    pub provenance: Value,
}
#[async_trait]
pub trait InputContextProvider: Send + Sync {
    async fn prepare(&self, workspace: &Path, messages: &[Message]) -> Result<PreparedInput>;
    /// Execution-scoped host services. Existing context providers need no changes.
    async fn prepare_for_execution(
        &self,
        workspace: &Path,
        messages: &[Message],
        system_prefix_len: usize,
        _context: &crate::TurnExecutionContext,
    ) -> Result<PreparedInput> {
        self.prepare_with_prefix(workspace, messages, system_prefix_len)
            .await
    }
    /// The declared host prefix boundary; leading historical system records are not host input.
    /// Existing providers keep their legacy behavior until they override this method.
    async fn prepare_with_prefix(
        &self,
        workspace: &Path,
        messages: &[Message],
        system_prefix_len: usize,
    ) -> Result<PreparedInput> {
        ensure!(
            system_prefix_len <= messages.len(),
            "invalid runtime prefix boundary"
        );
        validate_prefix(&messages[..system_prefix_len])?;
        self.prepare(workspace, messages).await
    }
}
pub(crate) fn validate_prefix(prepared: &[Message]) -> Result<()> {
    ensure!(
        prepared
            .iter()
            .all(|message| message.role == MessageRole::System),
        "input context may supply only a system prefix"
    );
    Ok(())
}
