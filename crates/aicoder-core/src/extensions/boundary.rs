//! Actionable successful-round/settlement boundaries, separate from notification observers.
use super::*;
use crate::model::*;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
pub const LIFECYCLE_NAMESPACE: &str = "aicoder.lifecycle.v1";
pub const MAX_BOUNDARY_BYTES: usize = 64 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryKind {
    BeforeStart,
    TurnEnd,
    BeforeSettle,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BoundaryEntry {
    /// Inert extension data; never model instructions or restored authority.
    Data { key: String, value: Value },
    /// An explicit native context message, tagged as host/extension source, never user input.
    Context { kind: String, text: String },
}
impl BoundaryEntry {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Data { key, value } => {
                super::name(key)?;
                size(value, 32 * 1024)?;
            }
            Self::Context { kind, text } => {
                super::name(kind)?;
                anyhow::ensure!(!text.is_empty(), "empty boundary context");
                size(text, 32 * 1024)?;
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundaryProposal {
    pub extension: String,
    pub hook: String,
    pub entry: BoundaryEntry,
}
pub struct BoundaryInput<'a> {
    pub workspace: &'a Path,
    pub kind: BoundaryKind,
    pub round: usize,
    pub history: &'a [Message],
    pub statistics: &'a BoundaryStatistics,
    pub context: &'a [Message],
    pub entries: &'a [BoundaryProposal],
    pub continue_requested: bool,
    pub can_continue: bool,
}
#[derive(Debug, Clone)]
pub struct BoundaryStatistics {
    pub rounds: usize,
    pub tool_calls: usize,
    pub usage: ModelUsage,
    pub primary_usage: Option<ModelUsage>,
    pub auxiliary: Vec<crate::auxiliary::AuxiliaryAttempt>,
    pub tokens: crate::run::TokenAccounting,
    pub output_bytes: usize,
    pub elapsed: std::time::Duration,
}
#[derive(Debug, Default)]
pub struct BoundaryAction {
    /// Replacements adopt this handler as the source; appends retain prior proposal provenance.
    pub replace: Option<Vec<BoundaryEntry>>,
    pub append: Vec<BoundaryEntry>,
    pub continue_run: Option<bool>,
}
#[async_trait]
pub trait BoundaryHook: Send + Sync {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<Option<BoundaryAction>>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundaryChange {
    pub extension: String,
    pub hook: String,
    pub replaced: bool,
    pub appended: usize,
    pub continue_run: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundaryCommit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<StartupMetadata>,
    pub kind: BoundaryKind,
    pub round: usize,
    pub base_index: usize,
    pub entries: Vec<BoundaryProposal>,
    pub continue_run: bool,
    pub changes: Vec<BoundaryChange>,
}
impl BoundaryCommit {
    pub(crate) fn session_entries(&self) -> anyhow::Result<Vec<crate::session::SessionEntryData>> {
        use crate::session::{NativeContextSource, SessionEntryData};
        let mut entries = vec![SessionEntryData::Extension {
            namespace: LIFECYCLE_NAMESPACE.into(),
            data: serde_json::to_value(LifecycleRecord::Applied {
                commit: self.clone(),
            })?,
        }];
        for proposal in &self.entries {
            if let BoundaryEntry::Context { kind, text } = &proposal.entry {
                entries.push(SessionEntryData::native_context(
                    NativeContextSource {
                        extension: proposal.extension.clone(),
                        hook: proposal.hook.clone(),
                        kind: kind.clone(),
                    },
                    text,
                )?);
            }
        }
        Ok(entries)
    }
    /// Canonical messages delivered by this explicitly sourced host action, not metadata replay.
    pub fn messages(&self) -> Vec<Message> {
        self.entries
            .iter()
            .filter_map(|p| match &p.entry {
                BoundaryEntry::Context { kind, text } => Some(
                    crate::session::NativeContextSource {
                        extension: p.extension.clone(),
                        hook: p.hook.clone(),
                        kind: kind.clone(),
                    }
                    .message(text),
                ),
                BoundaryEntry::Data { .. } => None,
            })
            .collect()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LifecycleRecord {
    Started { hooks: Vec<String> },
    Applied { commit: BoundaryCommit },
}
/// Summary normalization preserves explicit native context, while still excluding recovery views.
pub(crate) fn explicit_context_message(message: &Message) -> bool {
    message.role == MessageRole::System
        && message.content.len() == 1
        && matches!(&message.content[0],ContentBlock::Text{text}if text.starts_with("[aicoder extension ")&&text.split_once("]\n").is_some_and(|(label,_)|label.strip_prefix("[aicoder extension ").is_some_and(|label|label.split('/').count()==3&&label.split('/').all(|s|super::name(s).is_ok()))))
}
