use crate::{
    model::{ContentBlock, Message, MessageRole, ModelUsage},
    run::{PartialModelResponse, RunProgress, RunStop, TokenAccounting},
};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEntryData {
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
    pub stop: Option<RunStop>,
    pub partial_response: Option<PartialModelResponse>,
    /// Absent when a process disappeared before it could record final accounting.
    pub statistics: Option<TurnStatistics>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnStatistics {
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
            stop,
            partial_response: progress.partial_response.clone(),
            statistics: Some(TurnStatistics {
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
#[derive(Debug, Clone)]
pub(crate) struct StorageCursor {
    pub path: PathBuf,
    pub expected_len: u64,
    pub valid_len: u64,
    pub fingerprint: u64,
    pub needs_separator: bool,
}
#[derive(Debug, Clone)]
pub struct Session {
    pub(crate) metadata: SessionMetadata,
    pub(crate) entries: Vec<SessionEntry>,
    pub(crate) index: HashMap<String, usize>,
    pub(crate) storage: Option<StorageCursor>,
    pub(crate) run_token: Option<String>,
}
impl Session {
    /// Reconstruct a validated domain snapshot in a custom repository.
    /// Storage cursors and run ownership are never restored from serialized history.
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
    pub(crate) fn empty(metadata: SessionMetadata) -> Self {
        Self {
            metadata,
            entries: Vec::new(),
            index: HashMap::new(),
            storage: None,
            run_token: None,
        }
    }
    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }
    pub fn entries(&self) -> &[SessionEntry] {
        &self.entries
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
                SessionEntryData::Message { message } => Some(SessionMessage {
                    id: &entry.id,
                    parent_id: entry.parent_id.as_deref(),
                    created_at: entry.created_at,
                    message,
                }),
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
        self.storage.as_ref().map(|storage| storage.path.as_path())
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
        let mut output = Vec::new();
        let mut pending: Option<PendingTools> = None;
        for entry in self.active_entries() {
            match &entry.data {
                SessionEntryData::Message { message } => {
                    if message.role == MessageRole::Tool {
                        let tools = pending.as_mut().ok_or_else(|| {
                            anyhow::anyhow!("orphan tool result in session history")
                        })?;
                        for block in &message.content {
                            let ContentBlock::ToolResult { call_id, .. } = block else {
                                bail!("tool message contains a non-result block");
                            };
                            ensure!(
                                tools.ids.contains(call_id),
                                "tool result refers to an unknown call"
                            );
                            ensure!(
                                tools
                                    .results
                                    .insert(call_id.clone(), block.clone())
                                    .is_none(),
                                "duplicate tool result in session history"
                            );
                        }
                    } else {
                        flush_tools(&mut pending, &mut output);
                        let calls = message
                            .tool_calls()
                            .map(|call| call.id.clone())
                            .collect::<Vec<_>>();
                        if !calls.is_empty() {
                            ensure!(
                                message.role == MessageRole::Assistant,
                                "tool calls require an assistant message"
                            );
                            let ids = calls.iter().cloned().collect::<HashSet<_>>();
                            ensure!(
                                ids.len() == calls.len(),
                                "duplicate tool call IDs in session history"
                            );
                            pending = Some(PendingTools {
                                order: calls,
                                ids,
                                results: HashMap::new(),
                            });
                        }
                        output.push(message.clone());
                    }
                }
                SessionEntryData::TurnEnd { record, .. } => {
                    if record.stop.is_none() {
                        ensure!(
                            pending
                                .as_ref()
                                .is_none_or(|tools| tools.order.len() == tools.results.len()),
                            "completed turn has unfinished tool calls"
                        );
                    }
                    flush_tools(&mut pending, &mut output);
                    if record.stop.is_some() {
                        output.push(interruption_message());
                    }
                }
                _ => {}
            }
        }
        flush_tools(&mut pending, &mut output);
        if self.unfinished_turn().is_some() {
            output.push(interruption_message());
        }
        Ok(output)
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
                SessionEntryData::Message { message } => ensure!(
                    message.role != MessageRole::System,
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
                SessionEntryData::Extension { namespace, .. } => ensure!(
                    !namespace.is_empty(),
                    "extension namespace must not be empty"
                ),
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
            self.index.insert(entry.id.clone(), self.entries.len());
            self.entries.push(entry);
        }
        self.metadata.leaf_id = transaction.leaf_id;
        self.metadata.revision = revision;
        self.metadata.updated_at = updated_at;
    }
}
struct PendingTools {
    order: Vec<String>,
    ids: HashSet<String>,
    results: HashMap<String, ContentBlock>,
}
fn interruption_message() -> Message {
    Message::text(
        MessageRole::System,
        "[Previous execution was interrupted. Recorded results remain authoritative; unrecorded operations may have run partially. Inspect the workspace before retrying. Partial response fragments are not a completed answer.]",
    )
}
fn flush_tools(pending: &mut Option<PendingTools>, output: &mut Vec<Message>) {
    if let Some(mut tools) = pending.take() {
        for id in tools.order {
            let block=tools.results.remove(&id).unwrap_or_else(||ContentBlock::ToolResult {call_id:id,content:json!({"ok":false,"error":{"code":"execution_interrupted","message":"No completed result was recorded. The operation may have run partially; inspect the workspace before retrying."}}),is_error:true});
            output.push(Message {
                role: MessageRole::Tool,
                content: vec![block],
            });
        }
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
