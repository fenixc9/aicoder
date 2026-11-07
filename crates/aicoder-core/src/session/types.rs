use crate::{
    model::{Message, MessageRole, ModelUsage},
    run::{PartialModelResponse, RunProgress, RunStop, TokenAccounting},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    any::Any,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub version: u32,
    pub id: String,
    pub cwd: PathBuf,
    pub created_at: u64,
    pub updated_at: u64,
    pub title: Option<String>,
    pub leaf_id: Option<String>,
    pub revision: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub created_at: u64,
    pub data: SessionEntryData,
}
#[derive(Debug, Clone)]
pub enum SessionEntryData {
    CustomMessage {
        custom: crate::CustomMessage,
    },
    NativeContext {
        source: super::NativeContextSource,
        message: Message,
    },
    Message {
        message: Message,
    },
    ModelChanged {
        provider: String,
        model: String,
    },
    Compaction {
        strategy: String,
        summary: String,
        preserve_from: Option<String>,
        usage: ModelUsage,
    },
    Extension {
        namespace: String,
        data: Value,
    },
    TurnStart {
        turn_id: String,
    },
    TurnEnd {
        turn_id: String,
        record: Box<TurnRecord>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "SessionEntryData", tag = "kind", rename_all = "snake_case")]
pub(super) enum StoredSessionEntryData {
    #[serde(skip)]
    CustomMessage {
        custom: crate::CustomMessage,
    },
    #[serde(skip)]
    NativeContext {
        source: super::NativeContextSource,
        message: Message,
    },
    Message {
        message: Message,
    },
    ModelChanged {
        provider: String,
        model: String,
    },
    Compaction {
        strategy: String,
        summary: String,
        preserve_from: Option<String>,
        usage: ModelUsage,
    },
    Extension {
        namespace: String,
        data: Value,
    },
    TurnStart {
        turn_id: String,
    },
    TurnEnd {
        turn_id: String,
        record: Box<TurnRecord>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_inputs: Vec<crate::RunInput>,
    pub stop: Option<RunStop>,
    pub partial_response: Option<PartialModelResponse>,
    /// Absent when a process disappeared before it could record final accounting.
    pub statistics: Option<TurnStatistics>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnStatistics {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auxiliary: Vec<crate::auxiliary::AuxiliaryAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_usage: Option<ModelUsage>,
    pub usage: ModelUsage,
    pub rounds: usize,
    pub tool_calls: usize,
    pub output_bytes: usize,
    pub tokens: TokenAccounting,
    pub elapsed: Duration,
}
impl TurnRecord {
    pub fn from_progress(progress: &RunProgress, stop: Option<RunStop>) -> Self {
        Self {
            pending_inputs: progress.pending_inputs.clone(),
            stop,
            partial_response: progress.partial_response.clone(),
            statistics: Some(TurnStatistics {
                auxiliary: progress.auxiliary.clone(),
                primary_usage: progress.primary_usage.clone(),
                usage: progress.usage.clone(),
                rounds: progress.rounds,
                tool_calls: progress.tool_calls,
                output_bytes: progress.output_bytes,
                tokens: progress.tokens.clone(),
                elapsed: progress.elapsed,
            }),
        }
    }
    pub fn recovered() -> Self {
        Self {
            pending_inputs: Vec::new(),
            stop: Some(RunStop::Cancelled {
                reason: "Previous process ended without recording a terminal outcome".into(),
            }),
            partial_response: None,
            statistics: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTransaction {
    pub entries: Vec<SessionEntry>,
    pub leaf_id: Option<String>,
}
#[derive(Debug, Clone, Copy)]
pub struct SessionMessage<'a> {
    pub id: &'a str,
    pub parent_id: Option<&'a str>,
    pub created_at: u64,
    pub message: &'a Message,
}
#[derive(Clone)]
struct BackendState {
    value: Arc<dyn Any + Send + Sync>,
    path: Option<PathBuf>,
}
impl std::fmt::Debug for BackendState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendState")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone)]
pub struct Session {
    pub(crate) metadata: SessionMetadata,
    pub(crate) entries: Vec<SessionEntry>,
    pub(crate) index: HashMap<String, usize>,
    pub(crate) navigation_labels: BTreeMap<String, String>,
    backend: Option<BackendState>,
    pub(crate) run_token: Option<String>,
}
impl Session {
    /// Reconstruct a validated domain snapshot in a custom repository.
    /// Backend checkpoints and run ownership are never restored from serialized history.
    pub fn from_entries(metadata: SessionMetadata, entries: Vec<SessionEntry>) -> Result<Self> {
        ensure!(!metadata.id.is_empty(), "session ID must not be empty");
        ensure!(
            (1..=super::SESSION_VERSION).contains(&metadata.version),
            "unsupported session version"
        );
        ensure!(
            metadata.cwd.is_absolute(),
            "session workspace must be absolute"
        );
        let transaction = SessionTransaction {
            entries,
            leaf_id: metadata.leaf_id.clone(),
        };
        let revision = metadata.revision;
        let timestamp = metadata.updated_at;
        let mut session = Self::empty(metadata);
        session.metadata.leaf_id = None;
        session.validate(&transaction)?;
        session.apply(transaction, revision, timestamp);
        Ok(session)
    }
    /// Validate an atomic batch without changing the snapshot.
    pub fn validate_transaction(&self, transaction: &SessionTransaction) -> Result<()> {
        self.validate(transaction)
    }
    /// Fold a successfully persisted batch. Revisions must increase by exactly one.
    /// Repository implementations should call this only after their durable commit succeeds.
    pub fn apply_transaction(
        &mut self,
        transaction: SessionTransaction,
        revision: u64,
        updated_at: u64,
    ) -> Result<()> {
        ensure!(
            self.metadata.revision.checked_add(1) == Some(revision),
            "non-monotonic session revision"
        );
        self.validate(&transaction)?;
        self.apply(transaction, revision, updated_at);
        Ok(())
    }
    /// Ephemeral ownership capability for repository implementations, never serialized.
    pub fn run_token(&self) -> Option<&str> {
        self.run_token.as_deref()
    }
    /// Attach ownership after acquiring a run lease; clear it when reopening stored snapshots.
    pub fn attach_run_token(&mut self, token: Option<String>) {
        self.run_token = token;
    }
    /// Attach an immutable backend checkpoint to this view. Clones retain their old checkpoint.
    pub fn attach_backend_state<T: Any + Send + Sync>(&mut self, state: T, path: Option<PathBuf>) {
        self.backend = Some(BackendState {
            value: Arc::new(state),
            path,
        });
    }
    pub fn backend_state<T: Any>(&self) -> Option<&T> {
        self.backend.as_ref()?.value.downcast_ref()
    }
    pub fn clear_backend_state(&mut self) {
        self.backend = None;
    }
    pub(crate) fn empty(metadata: SessionMetadata) -> Self {
        Self {
            metadata,
            entries: Vec::new(),
            index: HashMap::new(),
            navigation_labels: BTreeMap::new(),
            backend: None,
            run_token: None,
        }
    }
    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }
    pub fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }
    pub fn entry_position(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }
    pub fn entry(&self, id: &str) -> Option<&SessionEntry> {
        self.index.get(id).map(|index| &self.entries[*index])
    }
    pub fn active_entries(&self) -> Vec<&SessionEntry> {
        let mut entries = Vec::new();
        let mut parent = self.metadata.leaf_id.as_deref();
        while let Some(id) = parent {
            let entry = self.entry(id).expect("validated session ancestry");
            entries.push(entry);
            parent = entry.parent_id.as_deref();
        }
        entries.reverse();
        entries
    }
    pub fn messages(&self) -> Vec<SessionMessage<'_>> {
        self.active_entries()
            .into_iter()
            .filter_map(|entry| match &entry.data {
                data if data.as_message().is_some() => {
                    let message = data.as_message().expect("message");
                    Some(SessionMessage {
                        id: &entry.id,
                        parent_id: entry.parent_id.as_deref(),
                        created_at: entry.created_at,
                        message,
                    })
                }
                _ => None,
            })
            .collect()
    }
    /// Raw messages from the selected branch; unfinished tool units may be present.
    pub fn chat_messages(&self) -> Vec<Message> {
        self.messages()
            .into_iter()
            .map(|entry| entry.message.clone())
            .collect()
    }
    pub fn path(&self) -> Option<&Path> {
        self.backend
            .as_ref()
            .and_then(|backend| backend.path.as_deref())
    }
    pub fn selected_model(&self) -> Option<(&str, &str)> {
        self.active_entries()
            .into_iter()
            .rev()
            .find_map(|entry| match &entry.data {
                SessionEntryData::ModelChanged { provider, model } => {
                    Some((provider.as_str(), model.as_str()))
                }
                _ => None,
            })
    }
    pub fn unfinished_turn(&self) -> Option<&str> {
        let mut active = None;
        for entry in self.active_entries() {
            match &entry.data {
                SessionEntryData::TurnStart { turn_id } => active = Some(turn_id.as_str()),
                SessionEntryData::TurnEnd { turn_id, .. } if active == Some(turn_id.as_str()) => {
                    active = None
                }
                _ => {}
            }
        }
        active
    }
    pub fn model_messages(&self) -> Result<Vec<Message>> {
        self.selected_branch()
            .model_context()
            .map(|context| context.messages)
    }
    pub(crate) fn transaction(&self, data: Vec<SessionEntryData>) -> Result<SessionTransaction> {
        let mut leaf = self.metadata.leaf_id.clone();
        let timestamp = super::now_millis()?;
        let mut entries = Vec::with_capacity(data.len());
        for data in data {
            let id = uuid::Uuid::new_v4().to_string();
            entries.push(SessionEntry {
                id: id.clone(),
                parent_id: leaf,
                created_at: timestamp,
                data,
            });
            leaf = Some(id);
        }
        Ok(SessionTransaction {
            entries,
            leaf_id: leaf,
        })
    }
    pub(crate) fn validate(&self, transaction: &SessionTransaction) -> Result<()> {
        let mut navigation_labels: Option<HashMap<String, String>> = None;
        let mut ids = HashSet::new();
        let mut new_entries: HashMap<&str, &SessionEntry> = HashMap::new();
        for entry in &transaction.entries {
            ensure!(
                !entry.id.is_empty()
                    && !self.index.contains_key(&entry.id)
                    && ids.insert(entry.id.as_str()),
                "duplicate or empty session entry ID"
            );
            ensure!(
                entry
                    .parent_id
                    .as_ref()
                    .is_none_or(|parent| self.index.contains_key(parent)
                        || ids.contains(parent.as_str()) && parent != &entry.id),
                "session parent must precede its child"
            );
            match &entry.data {
                SessionEntryData::Extension { namespace, .. }
                    if namespace == super::NATIVE_CONTEXT_NAMESPACE
                        || namespace == crate::CUSTOM_MESSAGE_NAMESPACE =>
                {
                    anyhow::bail!("reserved message namespace requires a typed entry")
                }
                SessionEntryData::CustomMessage { custom } => custom.validate()?,
                SessionEntryData::NativeContext { source, message } => source.validate(message)?,
                SessionEntryData::Message { message } => ensure!(
                    !matches!(message.role, MessageRole::System | MessageRole::Custom),
                    "System messages are runtime context and cannot be stored in a session"
                ),
                SessionEntryData::ModelChanged { provider, model } => ensure!(
                    !provider.is_empty() && !model.is_empty(),
                    "model identity must not be empty"
                ),
                SessionEntryData::Compaction {
                    strategy,
                    preserve_from,
                    ..
                } => {
                    ensure!(
                        !strategy.is_empty(),
                        "compaction strategy must not be empty"
                    );
                    if let Some(id) = preserve_from {
                        let mut parent = entry.parent_id.as_deref();
                        let mut found = false;
                        while let Some(current) = parent {
                            if current == id {
                                found = true;
                                break;
                            }
                            let ancestor = self
                                .entry(current)
                                .or_else(|| new_entries.get(current).copied())
                                .expect("validated parent");
                            parent = ancestor.parent_id.as_deref();
                        }
                        ensure!(found, "compaction must reference an ancestor");
                    }
                }
                SessionEntryData::TurnStart { turn_id }
                | SessionEntryData::TurnEnd { turn_id, .. } => {
                    ensure!(!turn_id.is_empty(), "turn ID must not be empty");
                    let mut parent = entry.parent_id.as_deref();
                    let mut open = None;
                    while let Some(id) = parent {
                        let ancestor = self
                            .entry(id)
                            .or_else(|| new_entries.get(id).copied())
                            .expect("validated parent");
                        match &ancestor.data {
                            SessionEntryData::TurnStart { turn_id } => {
                                open = Some(turn_id);
                                break;
                            }
                            SessionEntryData::TurnEnd { .. } => break,
                            _ => {}
                        }
                        parent = ancestor.parent_id.as_deref();
                    }
                    match &entry.data {
                        SessionEntryData::TurnStart { .. } => {
                            ensure!(open.is_none(), "previous turn is unfinished")
                        }
                        SessionEntryData::TurnEnd { .. } => ensure!(
                            open == Some(turn_id),
                            "turn end does not match an unfinished turn"
                        ),
                        _ => unreachable!(),
                    }
                }
                SessionEntryData::Extension { namespace, data } => {
                    if namespace == super::REASONING_NAMESPACE {
                        let choice: super::ReasoningSelection =
                            serde_json::from_value(data.clone())?;
                        choice.validate()?;
                        let mut parent = entry.parent_id.as_deref();
                        let mut model = None;
                        while let Some(id) = parent {
                            let ancestor = self
                                .entry(id)
                                .or_else(|| new_entries.get(id).copied())
                                .expect("validated parent");
                            if let SessionEntryData::ModelChanged {
                                provider,
                                model: identity,
                            } = &ancestor.data
                            {
                                model = Some((provider.as_str(), identity.as_str()));
                                break;
                            }
                            parent = ancestor.parent_id.as_deref();
                        }
                        ensure!(
                            model == Some((choice.provider.as_str(), choice.model.as_str())),
                            "reasoning selection requires its model epoch"
                        );
                    }

                    ensure!(
                        !namespace.is_empty(),
                        "extension namespace must not be empty"
                    );
                    if namespace == super::CONTEXT_EDIT_NAMESPACE {
                        let record: super::ContextEditRecord =
                            serde_json::from_value(data.clone())?;
                        let target = self
                            .entry(&record.target_id)
                            .or_else(|| new_entries.get(record.target_id.as_str()).copied())
                            .ok_or_else(|| anyhow::anyhow!("context edit target missing"))?;
                        let mut cursor = entry.parent_id.as_deref();
                        let ancestors = std::iter::from_fn(|| {
                            let id = cursor?;
                            let e = self
                                .entry(id)
                                .or_else(|| new_entries.get(id).copied())
                                .expect("validated ancestry");
                            cursor = e.parent_id.as_deref();
                            Some(id)
                        });
                        record.validate(target, ancestors)?;
                    }
                    if namespace == super::NAVIGATION_NAMESPACE {
                        let record: super::NavigationRecord = serde_json::from_value(data.clone())?;
                        record.validate(|id| {
                            self.index.contains_key(id) || new_entries.contains_key(id)
                        })?;
                        if !matches!(record, super::NavigationRecord::Rename { .. }) {
                            let labels = navigation_labels.get_or_insert_with(|| {
                                self.labels()
                                    .iter()
                                    .map(|(id, label)| (id.to_string(), label.to_string()))
                                    .collect()
                            });
                            match &record {
                                super::NavigationRecord::Branch { name: Some(name) } => {
                                    labels.insert(entry.id.clone(), name.clone());
                                }
                                super::NavigationRecord::Label {
                                    entry_id,
                                    label: Some(name),
                                } => {
                                    labels.insert(entry_id.clone(), name.clone());
                                }
                                super::NavigationRecord::Label {
                                    entry_id,
                                    label: None,
                                } => {
                                    labels.remove(entry_id);
                                }
                                _ => {}
                            }
                            ensure!(labels.len() <= 256, "session label limit is 256");
                        }
                    }
                    if namespace == crate::auxiliary::AUXILIARY_NAMESPACE {
                        serde_json::from_value::<crate::auxiliary::AuxiliaryRecord>(data.clone())?;
                        crate::extensions::size(data, 64 * 1024)?;
                    }
                    if namespace == crate::summary::MANUAL_USAGE_NAMESPACE {
                        let usage: crate::summary::manual::ManualSummaryUsage =
                            serde_json::from_value(data.clone())?;
                        usage.validate()?;
                    }
                    if namespace == crate::summary::NATIVE_COMPACTION_NAMESPACE {
                        let record: crate::summary::NativeCompactionRecord =
                            serde_json::from_value(data.clone())?;
                        let mut ancestors = Vec::new();
                        let mut cursor = entry.parent_id.as_deref();
                        while let Some(id) = cursor {
                            let ancestor = self
                                .entry(id)
                                .or_else(|| new_entries.get(id).copied())
                                .expect("validated parent");
                            ancestors.push(ancestor);
                            cursor = ancestor.parent_id.as_deref();
                        }
                        ancestors.reverse();
                        record.validate(&ancestors, entry.parent_id.as_deref())?;
                    }
                    if namespace == crate::summary::SUMMARY_NAMESPACE {
                        let checkpoint: crate::summary::SummaryCheckpoint =
                            serde_json::from_value(data.clone())?;
                        let mut ancestors = Vec::new();
                        let mut cursor = entry.parent_id.as_deref();
                        while let Some(id) = cursor {
                            let ancestor = self
                                .entry(id)
                                .or_else(|| new_entries.get(id).copied())
                                .expect("validated parent");
                            ancestors.push(ancestor);
                            cursor = ancestor.parent_id.as_deref();
                        }
                        ancestors.reverse();
                        crate::summary::validate_checkpoint(
                            &checkpoint,
                            &ancestors,
                            entry.parent_id.as_deref(),
                        )?;
                    }
                }
            }
            new_entries.insert(entry.id.as_str(), entry);
        }
        ensure!(
            transaction
                .leaf_id
                .as_ref()
                .is_none_or(|leaf| self.index.contains_key(leaf) || ids.contains(leaf.as_str())),
            "selected leaf does not exist"
        );
        Ok(())
    }
    pub(crate) fn apply(
        &mut self,
        transaction: SessionTransaction,
        revision: u64,
        updated_at: u64,
    ) {
        for entry in transaction.entries {
            if self.metadata.title.is_none()
                && let SessionEntryData::Message { message } = &entry.data
                && message.role == MessageRole::User
            {
                let title = message
                    .text_content()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !title.is_empty() {
                    self.metadata.title = Some(title.chars().take(80).collect());
                }
            }
            if let SessionEntryData::Extension { namespace, data } = &entry.data
                && namespace == super::NAVIGATION_NAMESPACE
            {
                match serde_json::from_value::<super::NavigationRecord>(data.clone())
                    .expect("validated navigation")
                {
                    super::NavigationRecord::Rename { title } => self.metadata.title = Some(title),
                    super::NavigationRecord::Branch { name: Some(name) } => {
                        self.navigation_labels.insert(entry.id.clone(), name);
                    }
                    super::NavigationRecord::Label {
                        entry_id,
                        label: Some(name),
                    } => {
                        self.navigation_labels.insert(entry_id, name);
                    }
                    super::NavigationRecord::Label {
                        entry_id,
                        label: None,
                    } => {
                        self.navigation_labels.remove(&entry_id);
                    }
                    _ => {}
                }
            }
            self.index.insert(entry.id.clone(), self.entries.len());
            self.entries.push(entry);
        }
        self.metadata.leaf_id = transaction.leaf_id;
        self.metadata.revision = revision;
        self.metadata.updated_at = updated_at;
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub cwd: PathBuf,
    pub created_at: u64,
    pub updated_at: u64,
    pub title: Option<String>,
    pub message_count: usize,
    pub entry_count: usize,
    pub leaf_id: Option<String>,
    pub path: Option<PathBuf>,
}
impl From<&Session> for SessionInfo {
    fn from(session: &Session) -> Self {
        Self {
            id: session.metadata.id.clone(),
            cwd: session.metadata.cwd.clone(),
            created_at: session.metadata.created_at,
            updated_at: session.metadata.updated_at,
            title: session.metadata.title.clone(),
            message_count: session.messages().len(),
            entry_count: session.entries.len(),
            leaf_id: session.metadata.leaf_id.clone(),
            path: session.path().map(Path::to_path_buf),
        }
    }
}
