//! Idle session navigation and bounded read-only views. No model or tool execution.
use super::*;
use crate::model::{ContentBlock, MessageRole};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};
pub const NAVIGATION_NAMESPACE: &str = "aicoder.session.navigation.v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum NavigationRecord {
    Rename {
        title: String,
    },
    Label {
        entry_id: String,
        label: Option<String>,
    },
    Branch {
        name: Option<String>,
    },
}
pub(crate) fn name(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.len() <= 256
            && !value
                .chars()
                .any(|ch| ch.is_control() || matches!(ch as u32,0x202a..=0x202e|0x2066..=0x2069)),
        "name must be nonempty, single-line and at most 256 bytes"
    );
    Ok(())
}
impl NavigationRecord {
    pub fn data(&self) -> SessionEntryData {
        SessionEntryData::Extension {
            namespace: NAVIGATION_NAMESPACE.into(),
            data: serde_json::to_value(self).expect("navigation"),
        }
    }
    pub(crate) fn validate(&self, exists: impl Fn(&str) -> bool) -> Result<()> {
        match self {
            Self::Rename { title } => name(title),
            Self::Label { entry_id, label } => {
                ensure!(
                    exists(entry_id),
                    "label target must already exist in this session"
                );
                if let Some(label) = label {
                    name(label)?;
                }
                Ok(())
            }
            Self::Branch { name: Some(label) } => name(label),
            Self::Branch { name: None } => Ok(()),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeCursor {
    pub session_id: String,
    pub revision: u64,
    pub offset: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub depth: usize,
    pub kind: String,
    pub selected: bool,
    pub on_selected_branch: bool,
    pub tip: bool,
    pub label: Option<String>,
    pub preview: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreePage {
    pub rows: Vec<TreeRow>,
    pub next: Option<TreeCursor>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchCursor {
    pub session_id: String,
    pub revision: u64,
    pub query: String,
    pub entry: usize,
    pub block: usize,
    pub offset: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub entry_id: String,
    pub role: MessageRole,
    pub snippet: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub next: Option<SearchCursor>,
    pub scanned_bytes: usize,
    pub scanned_entries: usize,
}
impl Session {
    /// Labels are navigation metadata over the entire tree, independent of selected ancestry.
    pub fn labels(&self) -> &BTreeMap<String, String> {
        &self.navigation_labels
    }
    /// Exact IDs win; duplicate labels are an explicit ambiguity, never a guessed branch.
    pub fn resolve_entry(&self, value: &str) -> Result<&str> {
        if let Some(entry) = self.entry(value) {
            return Ok(&entry.id);
        }
        let labels = self.labels();
        let mut found = labels
            .iter()
            .filter(|(_, label)| label.as_str() == value)
            .map(|(id, _)| id.as_str());
        let first = found
            .next()
            .ok_or_else(|| anyhow::anyhow!("entry or label not found"))?;
        ensure!(
            found.next().is_none(),
            "label is ambiguous; use an entry ID"
        );
        Ok(first)
    }
    pub fn tree_page(&self, limit: usize, cursor: Option<&TreeCursor>) -> Result<TreePage> {
        ensure!((1..=200).contains(&limit), "tree page limit must be 1..200");
        let offset = if let Some(cursor) = cursor {
            ensure!(
                cursor.session_id == self.metadata.id && cursor.revision == self.metadata.revision,
                "stale or foreign tree cursor"
            );
            cursor.offset
        } else {
            0
        };
        ensure!(offset <= self.entries.len(), "invalid tree offset");
        let selected = self
            .active_entries()
            .into_iter()
            .map(|entry| entry.id.as_str())
            .collect::<HashSet<_>>();
        let labels = self.labels();
        let mut depth = Vec::with_capacity(self.entries.len());
        let mut children = vec![false; self.entries.len()];
        for entry in &self.entries {
            let d = if let Some(parent) = &entry.parent_id {
                let index = self.index[parent];
                children[index] = true;
                depth[index] + 1
            } else {
                0
            };
            depth.push(d);
        }
        let end = offset.saturating_add(limit).min(self.entries.len());
        let rows = self.entries[offset..end]
            .iter()
            .enumerate()
            .map(|(local, entry)| {
                let (kind, preview) = match &entry.data {
                    SessionEntryData::Message { message }
                    | SessionEntryData::NativeContext { message, .. } => (
                        format!("{:?}", message.role).to_ascii_lowercase(),
                        message
                            .content
                            .iter()
                            .take(16)
                            .find_map(|block| {
                                if let ContentBlock::Text { text } = block {
                                    Some(text.chars().take(160).collect())
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default(),
                    ),
                    SessionEntryData::ModelChanged { provider, model } => (
                        "model_changed".into(),
                        provider
                            .chars()
                            .chain(['/'])
                            .chain(model.chars())
                            .take(160)
                            .collect(),
                    ),
                    SessionEntryData::TurnStart { .. } => ("turn_start".into(), String::new()),
                    SessionEntryData::TurnEnd { record, .. } => (
                        if record.stop.is_some() {
                            "interrupted"
                        } else {
                            "completed"
                        }
                        .into(),
                        String::new(),
                    ),
                    SessionEntryData::Compaction { .. } => {
                        ("legacy_compaction".into(), String::new())
                    }
                    SessionEntryData::Extension { namespace, .. } => {
                        ("extension".into(), namespace.chars().take(160).collect())
                    }
                };
                TreeRow {
                    id: entry.id.clone(),
                    parent_id: entry.parent_id.clone(),
                    depth: depth[offset + local],
                    kind,
                    selected: self.metadata.leaf_id.as_deref() == Some(&entry.id),
                    on_selected_branch: selected.contains(entry.id.as_str()),
                    tip: !children[offset + local],
                    label: labels
                        .get(entry.id.as_str())
                        .map(|value| (*value).to_string()),
                    preview,
                }
            })
            .collect();
        Ok(TreePage {
            rows,
            next: (end < self.entries.len()).then(|| TreeCursor {
                session_id: self.metadata.id.clone(),
                revision: self.metadata.revision,
                offset: end,
            }),
        })
    }
    /// Literal, case-sensitive search over recorded visible text. One hit per message.
    /// Each page examines at most 512 entries and 256 KiB of text, including overlap.
    pub fn search_messages(
        &self,
        query: &str,
        limit: usize,
        cursor: Option<&SearchCursor>,
    ) -> Result<SearchPage> {
        search_archive_entries(
            &self.metadata,
            self.entries.len(),
            query,
            limit,
            cursor,
            |index| Ok(std::borrow::Cow::Borrowed(&self.entries[index])),
        )
    }
}
/// Shared bounded visible-text scan for in-memory snapshots and paged archive readers.
pub fn search_archive_entries<'a>(
    metadata: &SessionMetadata,
    entry_count: usize,
    query: &str,
    limit: usize,
    cursor: Option<&SearchCursor>,
    mut read: impl FnMut(usize) -> Result<std::borrow::Cow<'a, SessionEntry>>,
) -> Result<SearchPage> {
    ensure!(
        !query.is_empty() && query.len() <= 256,
        "query must be 1..256 bytes"
    );
    ensure!((1..=100).contains(&limit), "search limit must be 1..100");
    let (mut index, mut block, mut offset) = if let Some(cursor) = cursor {
        ensure!(
            cursor.session_id == metadata.id
                && cursor.revision == metadata.revision
                && cursor.query == query,
            "stale, foreign or mismatched search cursor"
        );
        (cursor.entry, cursor.block, cursor.offset)
    } else {
        (0, 0, 0)
    };
    ensure!(index <= entry_count, "invalid search entry");
    let mut page = SearchPage {
        hits: Vec::new(),
        next: None,
        scanned_bytes: 0,
        scanned_entries: 0,
    };
    let mut blocks = 0;
    'entries: while index < entry_count
        && page.scanned_entries < 512
        && page.scanned_bytes < 256 * 1024
        && page.hits.len() < limit
        && blocks < 4096
    {
        let entry = read(index)?;
        page.scanned_entries += 1;
        if let Some(message) = entry.data.as_message() {
            ensure!(block <= message.content.len(), "invalid search block");
            while block < message.content.len() {
                if blocks >= 4096 || page.scanned_bytes >= 256 * 1024 {
                    break 'entries;
                }
                blocks += 1;
                if let ContentBlock::Text { text } = &message.content[block] {
                    ensure!(
                        offset <= text.len() && text.is_char_boundary(offset),
                        "invalid search byte offset"
                    );
                    let remaining = 256 * 1024 - page.scanned_bytes;
                    if remaining <= query.len() + 4 {
                        break 'entries;
                    }
                    let mut end = offset.saturating_add(remaining).min(text.len());
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    page.scanned_bytes += end - offset;
                    if let Some(found) = text[offset..end].find(query) {
                        let found = offset + found;
                        let mut left = found.saturating_sub(64);
                        while !text.is_char_boundary(left) {
                            left -= 1;
                        }
                        let snippet = text[left..].chars().take(160).collect();
                        page.hits.push(SearchHit {
                            entry_id: entry.id.clone(),
                            role: message.role,
                            snippet,
                        });
                        index += 1;
                        block = 0;
                        offset = 0;
                        continue 'entries;
                    }
                    if end < text.len() {
                        let mut next = end.saturating_sub(query.len().saturating_sub(1));
                        while !text.is_char_boundary(next) {
                            next -= 1;
                        }
                        ensure!(next > offset, "search cannot advance");
                        offset = next;
                        break 'entries;
                    }
                } else {
                    ensure!(offset == 0, "invalid non-text search offset");
                }
                block += 1;
                offset = 0;
            }
        } else {
            ensure!(block == 0 && offset == 0, "invalid metadata search cursor");
        }
        index += 1;
        block = 0;
        offset = 0;
    }
    if index < entry_count {
        page.next = Some(SearchCursor {
            session_id: metadata.id.clone(),
            revision: metadata.revision,
            query: query.into(),
            entry: index,
            block,
            offset,
        });
    }
    Ok(page)
}

/// Scope every idle operation to the host's canonical workspace and repository lease.
pub struct SessionNavigator<'a> {
    repository: &'a dyn SessionRepository,
    workspace: PathBuf,
}
impl<'a> SessionNavigator<'a> {
    pub fn new(repository: &'a dyn SessionRepository, workspace: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            repository,
            workspace: workspace.as_ref().canonicalize()?,
        })
    }
    pub fn open(&self, id: &str) -> Result<Session> {
        let session = self.repository.open(id)?;
        ensure!(
            session.metadata.cwd == self.workspace,
            "session belongs to another workspace"
        );
        Ok(clear_token(session))
    }
    pub fn open_run(&self, id: &str) -> Result<RunSession> {
        let session = self.repository.open_run(id)?;
        ensure!(
            session.metadata().cwd == self.workspace,
            "session belongs to another workspace"
        );
        Ok(session)
    }
    pub fn info(&self, id: &str) -> Result<SessionInfo> {
        let info = self.repository.session_info(id)?;
        ensure!(
            info.cwd == self.workspace,
            "session belongs to another workspace"
        );
        Ok(info)
    }
    pub fn tree(&self, id: &str, limit: usize, cursor: Option<&TreeCursor>) -> Result<TreePage> {
        self.info(id)?;
        self.repository.tree_page(id, limit, cursor)
    }
    pub fn search(
        &self,
        id: &str,
        query: &str,
        limit: usize,
        cursor: Option<&SearchCursor>,
    ) -> Result<SearchPage> {
        self.info(id)?;
        self.repository.search_page(id, query, limit, cursor)
    }
    pub fn archive(
        &self,
        id: &str,
        limit: usize,
        cursor: Option<&ArchiveCursor>,
    ) -> Result<ArchivePage> {
        self.info(id)?;
        self.repository.archive_page(id, limit, cursor)
    }
    /// Unified metadata mutations for full snapshots and bounded archive windows.
    pub fn execute(&self, id: &str, action: ArchiveNavigation) -> Result<RunSession> {
        let mut state = self.open_run(id)?;
        let lease = state.acquire(self.repository)?;
        match &mut state {
            RunSession::Window(window) => self.repository.navigate_window(window, action)?,
            RunSession::Full(session) => {
                let data = match action {
                    ArchiveNavigation::Rename { title } => Some(NavigationRecord::Rename { title }),
                    ArchiveNavigation::Label { entry_id, label } => {
                        let entry_id = session.resolve_entry(&entry_id)?.to_string();
                        Some(NavigationRecord::Label { entry_id, label })
                    }
                    ArchiveNavigation::Checkout { target } => {
                        let target = target
                            .as_deref()
                            .map(|target| session.resolve_entry(target).map(str::to_string))
                            .transpose()?;
                        self.repository.checkout(session, target.as_deref())?;
                        None
                    }
                    ArchiveNavigation::Branch { source, name } => {
                        let parent = match source {
                            BranchSource::Current => session.metadata().leaf_id.clone(),
                            BranchSource::Root => None,
                            BranchSource::Entry(target) => {
                                Some(session.resolve_entry(&target)?.to_string())
                            }
                        };
                        session.branch(parent.as_deref())?.model_context()?;
                        let entry = SessionEntry {
                            id: uuid::Uuid::new_v4().to_string(),
                            parent_id: parent,
                            created_at: super::now_millis()?,
                            data: NavigationRecord::Branch { name }.data(),
                        };
                        let transaction = SessionTransaction {
                            leaf_id: Some(entry.id.clone()),
                            entries: vec![entry],
                        };
                        self.repository.commit(session, transaction)?;
                        None
                    }
                    ArchiveNavigation::Recover => {
                        session.selected_branch().model_context()?;
                        if let Some(turn) = session.unfinished_turn().map(str::to_string) {
                            self.repository.append_entries(
                                session,
                                vec![SessionEntryData::TurnEnd {
                                    turn_id: turn,
                                    record: Box::new(TurnRecord::recovered()),
                                }],
                            )?;
                        }
                        None
                    }
                };
                if let Some(data) = data {
                    self.repository.append_entries(session, vec![data.data()])?;
                }
            }
        }
        drop(lease);
        state.clear_run_token();
        Ok(state)
    }
    pub fn rename(&self, id: &str, title: &str) -> Result<Session> {
        name(title)?;
        self.append(
            id,
            NavigationRecord::Rename {
                title: title.into(),
            },
        )
    }
    pub fn label(&self, id: &str, target: &str, label: Option<&str>) -> Result<Session> {
        let mut session = self.open(id)?;
        let target = session.resolve_entry(target)?.to_string();
        if let Some(label) = label {
            name(label)?;
        }
        let _lease = self.repository.acquire_run(&mut session)?;
        self.repository.append_entries(
            &mut session,
            vec![
                NavigationRecord::Label {
                    entry_id: target,
                    label: label.map(str::to_string),
                }
                .data(),
            ],
        )?;
        Ok(clear_token(session))
    }
    fn append(&self, id: &str, record: NavigationRecord) -> Result<Session> {
        let mut session = self.open(id)?;
        let _lease = self.repository.acquire_run(&mut session)?;
        self.repository
            .append_entries(&mut session, vec![record.data()])?;
        Ok(clear_token(session))
    }
    pub fn checkout(&self, id: &str, target: Option<&str>) -> Result<Session> {
        let mut session = self.open(id)?;
        let target = target
            .map(|value| session.resolve_entry(value).map(str::to_string))
            .transpose()?;
        let _lease = self.repository.acquire_run(&mut session)?;
        self.repository.checkout(&mut session, target.as_deref())?;
        Ok(clear_token(session))
    }
    pub fn branch(&self, id: &str, target: Option<&str>, label: Option<&str>) -> Result<Session> {
        if let Some(label) = label {
            name(label)?;
        }
        let mut session = self.open(id)?;
        let parent = target
            .map(|value| session.resolve_entry(value).map(str::to_string))
            .transpose()?;
        session.branch(parent.as_deref())?.model_context()?;
        let _lease = self.repository.acquire_run(&mut session)?;
        let entry = SessionEntry {
            id: uuid::Uuid::new_v4().to_string(),
            parent_id: parent,
            created_at: super::now_millis()?,
            data: NavigationRecord::Branch {
                name: label.map(str::to_string),
            }
            .data(),
        };
        let transaction = SessionTransaction {
            leaf_id: Some(entry.id.clone()),
            entries: vec![entry],
        };
        self.repository.commit(&mut session, transaction)?;
        Ok(clear_token(session))
    }
    pub fn recover(&self, id: &str) -> Result<Session> {
        let mut session = self.open(id)?;
        let _lease = self.repository.acquire_run(&mut session)?;
        session.selected_branch().model_context()?;
        if let Some(turn) = session.unfinished_turn().map(str::to_string) {
            self.repository.append_entries(
                &mut session,
                vec![SessionEntryData::TurnEnd {
                    turn_id: turn,
                    record: Box::new(TurnRecord::recovered()),
                }],
            )?;
        }
        Ok(clear_token(session))
    }
}

fn clear_token(mut session: Session) -> Session {
    session.attach_run_token(None);
    session
}
