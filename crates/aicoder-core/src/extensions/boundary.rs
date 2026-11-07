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
    CustomMessage {
        kind: String,
        content: Vec<ContentBlock>,
        display: bool,
        details: Option<Value>,
    },
    ContextEdit {
        target_id: String,
        replacement: crate::session::ContextReplacement,
    },
    Compaction {
        summary: String,
        first_kept_entry_id: Option<String>,
        details: Option<Value>,
    },
}
impl BoundaryEntry {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::CustomMessage { kind, content, .. } => {
                super::name(kind)?;
                crate::CustomMessage::new("validation", kind, content.clone())?.validate()?;
                size(self, 32 * 1024)?;
            }
            Self::ContextEdit { target_id, .. } => {
                anyhow::ensure!(
                    !target_id.is_empty() && target_id.len() <= 256,
                    "invalid context edit ID"
                );
                size(self, 32 * 1024)?;
            }
            Self::Compaction {
                summary, details, ..
            } => {
                anyhow::ensure!(!summary.trim().is_empty(), "empty boundary compaction");
                size(summary, 32 * 1024)?;
                if let Some(details) = details {
                    size(details, 32 * 1024)?;
                }
            }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    pub extension: String,
    pub hook: String,
    pub entry: BoundaryEntry,
}
pub struct BoundaryInput<'a> {
    pub workspace: &'a Path,
    pub session_context: Option<&'a crate::session::OwnedSessionContext>,
    pub context_preview: &'a [Message],
    pub models: &'a super::BoundaryModels<'a>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_finalizations: Vec<MessageFinalizationReceipt>,
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
    fn validate_receipts(&self) -> anyhow::Result<()> {
        if !self.message_finalizations.is_empty() {
            let messages = self.messages();
            anyhow::ensure!(
                self.message_finalizations.len() == messages.len(),
                "context finalization count mismatch"
            );
            for (receipt, message) in self.message_finalizations.iter().zip(&messages) {
                anyhow::ensure!(
                    receipt.role == message.role
                        && receipt.effective_bytes == size(message, 64 * 1024)?,
                    "context finalization message mismatch"
                );
            }
        }
        Ok(())
    }
    pub(crate) fn session_entries(&self) -> anyhow::Result<Vec<crate::session::SessionEntryData>> {
        use crate::session::{NativeContextSource, SessionEntryData};
        self.validate_receipts()?;
        let mut entries = vec![SessionEntryData::Extension {
            namespace: LIFECYCLE_NAMESPACE.into(),
            data: serde_json::to_value(LifecycleRecord::Applied {
                commit: self.clone(),
            })?,
        }];
        for receipt in &self.message_finalizations {
            entries.push(receipt_entry(receipt)?);
        }
        for proposal in &self.entries {
            if let Some(custom) = proposal.custom_message()? {
                entries.push(SessionEntryData::CustomMessage { custom });
            }
            anyhow::ensure!(
                !matches!(
                    proposal.entry,
                    BoundaryEntry::ContextEdit { .. } | BoundaryEntry::Compaction { .. }
                ),
                "projected boundary entries require an owned append plan"
            );
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
                BoundaryEntry::CustomMessage { .. } => p
                    .custom_message()
                    .expect("validated boundary custom message")
                    .map(|c| c.message().clone()),
                _ => None,
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

impl BoundaryProposal {
    pub(crate) fn staged(extension: String, hook: String, entry: BoundaryEntry) -> Self {
        let entry_id = if matches!(entry, BoundaryEntry::Data { .. }) {
            None
        } else {
            Some(uuid::Uuid::new_v4().to_string())
        };
        Self {
            extension,
            hook,
            entry,
            entry_id,
        }
    }

    pub fn custom_message(&self) -> anyhow::Result<Option<crate::CustomMessage>> {
        let BoundaryEntry::CustomMessage {
            kind,
            content,
            display,
            details,
        } = &self.entry
        else {
            return Ok(None);
        };
        let mut custom = crate::CustomMessage::new(&self.extension, kind, content.clone())?
            .with_display(*display);
        if let Some(details) = details {
            custom = custom.with_details(details.clone())?;
        }
        Ok(Some(custom))
    }
}
impl BoundaryCommit {
    /// In-memory journals can append messages and self-retaining reductions; ID edits need a session.
    pub fn ephemeral_view(
        &self,
        current: &[Message],
        prefix: usize,
    ) -> anyhow::Result<Vec<Message>> {
        anyhow::ensure!(prefix <= current.len(), "invalid boundary runtime prefix");
        let mut view = current.to_vec();
        for p in &self.entries {
            p.entry.validate()?;
            match &p.entry {
                BoundaryEntry::Data { .. } => {}
                BoundaryEntry::Context { kind, text } => view.push(
                    crate::session::NativeContextSource {
                        extension: p.extension.clone(),
                        hook: p.hook.clone(),
                        kind: kind.clone(),
                    }
                    .message(text),
                ),
                BoundaryEntry::CustomMessage { .. } => view.push(
                    p.custom_message()?
                        .expect("custom proposal")
                        .message()
                        .clone(),
                ),
                BoundaryEntry::Compaction {
                    summary,
                    first_kept_entry_id: None,
                    ..
                } => {
                    let uncertain = crate::summary::uncertain(&view[prefix..]);
                    view.truncate(prefix);
                    if uncertain {
                        view.push(crate::summary::recovery_notice());
                    }
                    view.push(
                        crate::summary::NativeCompactionRecord {
                            version: 1,
                            extension: p.extension.clone(),
                            hook: p.hook.clone(),
                            source_leaf: None,
                            source_revision: 0,
                            first_kept_entry_id: None,
                            replaced_messages: 0,
                            retained_messages: 0,
                            summary: summary.clone(),
                            details: None,
                            uncertain_operations: false,
                        }
                        .message(),
                    );
                }
                _ => anyhow::bail!("journal does not support ID-bound boundary drafts"),
            }
        }
        Ok(view)
    }
    pub fn changes_projection(&self) -> bool {
        self.entries.iter().any(|p| {
            matches!(
                p.entry,
                BoundaryEntry::ContextEdit { .. } | BoundaryEntry::Compaction { .. }
            )
        })
    }
    /// Backend preparation only: source must be the pinned full session or its private validation index.
    pub fn session_transaction(
        &self,
        session: &crate::session::Session,
        active_turn: Option<&str>,
    ) -> anyhow::Result<crate::session::SessionTransaction> {
        use crate::session::*;
        self.validate_receipts()?;
        let mut first = vec![SessionEntryData::Extension {
            namespace: LIFECYCLE_NAMESPACE.into(),
            data: serde_json::to_value(LifecycleRecord::Applied {
                commit: self.clone(),
            })?,
        }];
        for receipt in &self.message_finalizations {
            first.push(receipt_entry(receipt)?);
        }
        let branch = session.selected_branch();
        let source = match active_turn {
            Some(turn) => branch.context_for_active_turn(turn)?,
            None => branch.model_context()?,
        };
        let uncertain = source.source.interrupted_turns > 0
            || source.source.recovered_tool_results > 0
            || crate::summary::uncertain(&source.messages);
        let mut entries = Vec::<SessionEntry>::new();
        let mut parent = session.metadata().leaf_id.clone();
        let now = super::super::session::now_millis()?;
        let add = |data,
                   entries: &mut Vec<SessionEntry>,
                   parent: &mut Option<String>,
                   fixed: Option<&str>| {
            let id = fixed
                .map(str::to_owned)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            entries.push(SessionEntry {
                id: id.clone(),
                parent_id: parent.clone(),
                created_at: now,
                data,
            });
            *parent = Some(id);
        };
        for data in first {
            add(data, &mut entries, &mut parent, None);
        }
        for proposal in &self.entries {
            proposal.entry.validate()?;
            let data = match &proposal.entry {
                BoundaryEntry::Data { .. } => continue,
                BoundaryEntry::Context { kind, text } => SessionEntryData::native_context(
                    NativeContextSource {
                        extension: proposal.extension.clone(),
                        hook: proposal.hook.clone(),
                        kind: kind.clone(),
                    },
                    text,
                )?,
                BoundaryEntry::CustomMessage { .. } => SessionEntryData::CustomMessage {
                    custom: proposal.custom_message()?.expect("custom proposal"),
                },
                BoundaryEntry::ContextEdit {
                    target_id,
                    replacement,
                } => ContextEditRecord {
                    version: 1,
                    extension: proposal.extension.clone(),
                    hook: proposal.hook.clone(),
                    target_id: target_id.clone(),
                    replacement: replacement.clone(),
                }
                .data(),
                BoundaryEntry::Compaction {
                    summary,
                    first_kept_entry_id,
                    details,
                } => {
                    let branch = session.selected_branch();
                    let ancestors = branch
                        .entries()
                        .iter()
                        .copied()
                        .chain(entries.iter())
                        .collect::<Vec<_>>();
                    let start = match first_kept_entry_id {
                        Some(id) => ancestors
                            .iter()
                            .position(|e| &e.id == id)
                            .ok_or_else(|| anyhow::anyhow!("native compaction target missing"))?,
                        None => ancestors.len(),
                    };
                    crate::summary::NativeCompactionRecord {
                        version: 1,
                        extension: proposal.extension.clone(),
                        hook: proposal.hook.clone(),
                        source_leaf: parent.clone(),
                        source_revision: session.metadata().revision,
                        first_kept_entry_id: first_kept_entry_id.clone(),
                        replaced_messages: ancestors[..start]
                            .iter()
                            .filter(|e| e.data.as_message().is_some())
                            .count(),
                        retained_messages: ancestors[start..]
                            .iter()
                            .filter(|e| e.data.as_message().is_some())
                            .count(),
                        summary: summary.clone(),
                        details: details.clone(),
                        uncertain_operations: uncertain,
                    }
                    .data()
                }
            };
            add(
                data,
                &mut entries,
                &mut parent,
                proposal.entry_id.as_deref(),
            );
        }
        let transaction = SessionTransaction {
            entries,
            leaf_id: parent,
        };
        session.validate_transaction(&transaction)?;
        Ok(transaction)
    }
}
/// Opaque one-use journal preparation. Native backend bindings are never serialized into receipts.
pub struct PreparedBoundary {
    commit: BoundaryCommit,
    projection: Option<Vec<Message>>,
    backend: Option<Box<dyn std::any::Any + Send>>,
    session_projection: Option<crate::session::OwnedSessionContext>,
}
impl PreparedBoundary {
    pub fn new(commit: BoundaryCommit, projection: Option<Vec<Message>>) -> Self {
        Self {
            commit,
            projection,
            backend: None,
            session_projection: None,
        }
    }
    pub fn set_session_projection(&mut self, value: crate::session::OwnedSessionContext) {
        self.session_projection = Some(value);
    }
    pub fn session_projection(&self) -> Option<&crate::session::OwnedSessionContext> {
        self.session_projection.as_ref()
    }
    pub fn commit(&self) -> &BoundaryCommit {
        &self.commit
    }
    pub fn projection(&self) -> Option<&[Message]> {
        self.projection.as_deref()
    }
    pub fn bind<T: std::any::Any + Send>(&mut self, value: T) {
        self.backend = Some(Box::new(value));
    }
    pub fn into_parts<T: std::any::Any + Send>(
        self,
    ) -> anyhow::Result<(BoundaryCommit, Option<Vec<Message>>, T)> {
        let value = self
            .backend
            .ok_or_else(|| anyhow::anyhow!("missing prepared boundary backend"))?
            .downcast::<T>()
            .map_err(|_| anyhow::anyhow!("foreign prepared boundary backend"))?;
        Ok((self.commit, self.projection, *value))
    }
}
