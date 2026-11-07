//! Native before-compaction decisions and post-effect observations.
use super::*;
use crate::{model::Message, session::OwnedSessionContext};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    Automatic,
    Manual,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryInstructions {
    pub text: String,
    pub replace_default: bool,
}
impl SummaryInstructions {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        size(&self.text, 32 * 1024)?;
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionReplacement {
    pub summary: String,
    pub first_kept_entry_id: Option<String>,
    pub details: Option<Value>,
}
impl CompactionReplacement {
    pub(crate) fn entry(&self) -> BoundaryEntry {
        BoundaryEntry::Compaction {
            summary: self.summary.clone(),
            first_kept_entry_id: self.first_kept_entry_id.clone(),
            details: self.details.clone(),
        }
    }
}
#[derive(Debug, Default)]
pub struct CompactionAction {
    pub cancel: bool,
    pub instructions: Option<SummaryInstructions>,
    pub replacement: Option<CompactionReplacement>,
    /// Explicitly resume default generation after an earlier replacement.
    pub use_default: bool,
}
pub struct CompactionHookInput<'a> {
    pub workspace: &'a Path,
    pub trigger: CompactionTrigger,
    pub context: &'a [Message],
    pub history: &'a [Message],
    pub system_prefix_len: usize,
    pub session_context: Option<&'a OwnedSessionContext>,
    pub target_tokens: usize,
    pub instructions: Option<&'a SummaryInstructions>,
    pub replacement: Option<&'a CompactionReplacement>,
    pub models: &'a BoundaryModels<'a>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStatus {
    Completed,
    Unchanged,
    Vetoed,
    Cancelled,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEvent {
    pub trigger: CompactionTrigger,
    pub status: CompactionStatus,
    pub from_extension: bool,
    pub checkpoint_entry_id: Option<String>,
    pub before: usize,
    pub after: usize,
}
#[async_trait]
pub trait CompactionHook: Send + Sync {
    async fn before(
        &self,
        _input: CompactionHookInput<'_>,
        _cancellation: ExtensionCancellation,
    ) -> anyhow::Result<Option<CompactionAction>> {
        Ok(None)
    }
    async fn after(
        &self,
        _event: &CompactionEvent,
        _cancellation: ExtensionCancellation,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionDelivery {
    pub registered: usize,
    pub invoked: usize,
    pub completed: usize,
    pub failures: Vec<CompactionObservationFailure>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionObservationFailure {
    pub extension: String,
    pub hook: String,
    pub class: String,
}
