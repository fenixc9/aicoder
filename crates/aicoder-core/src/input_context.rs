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
