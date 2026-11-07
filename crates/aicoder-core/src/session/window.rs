//! Explicit materialized run windows over an immutable canonical archive.
use super::*;
use crate::model::Message;
use anyhow::{Result, ensure};
use std::{any::Any, path::PathBuf, sync::Arc};
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchiveCursor {
    pub session_id: String,
    pub revision: u64,
    pub offset: usize,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchivePage {
    pub metadata: SessionMetadata,
    pub entries: Vec<SessionEntry>,
    pub next: Option<ArchiveCursor>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ArchiveNavigation {
    /// Append a branch-local model-view edit without changing its canonical target.
    EditContext {
        edit: super::ContextEditRecord,
    },
    Rename {
        title: String,
    },
    Label {
        entry_id: String,
        label: Option<String>,
    },
    Checkout {
        target: Option<String>,
    },
    Branch {
        source: BranchSource,
        name: Option<String>,
    },
    Recover,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "entry", rename_all = "snake_case")]
pub enum BranchSource {
    Current,
    Root,
    Entry(String),
}
/// A bounded canonical tail and its separately projected model view. Omitted records
/// remain in the archive; they are never represented as fabricated Message values here.
#[derive(Clone)]
pub struct SessionWindow {
    metadata: SessionMetadata,
    entries: Vec<SessionEntry>,
    context: Vec<Message>,
    source: SessionContextSource,
    model: Option<(String, String)>,
    pub(super) reasoning: Option<ReasoningSelection>,
    unfinished: Option<String>,
    backend: Option<Arc<dyn Any + Send + Sync>>,
    run_token: Option<String>,
    context_revision: u64,
    accounting: BranchAccounting,
}
impl std::fmt::Debug for SessionWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionWindow")
            .field("metadata", &self.metadata)
            .field("loaded_entries", &self.entries.len())
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}
impl SessionWindow {
    /// Repository boundary: source counts describe the complete selected branch, while
    /// entries contain only real materialized records. Backend state/ownership are separate.
    pub fn new(
        metadata: SessionMetadata,
        entries: Vec<SessionEntry>,
        context: Vec<Message>,
        source: SessionContextSource,
        model: Option<(String, String)>,
        unfinished: Option<String>,
        accounting: BranchAccounting,
    ) -> Result<Self> {
        ensure!(
            source.session_id == metadata.id
                && source.revision == metadata.revision
                && source.leaf_id == metadata.leaf_id,
            "window source does not match its snapshot"
        );
        let loaded = entries
            .iter()
            .filter(|entry| entry.data.as_message().is_some())
            .count();
        ensure!(
            loaded.saturating_add(source.archived_messages) == source.recorded_messages,
            "window canonical message counts differ"
        );
        Ok(Self {
            context_revision: metadata.revision,
            metadata,
            entries,
            context,
            source,
            model,
            reasoning: None,
            unfinished,
            backend: None,
            run_token: None,
            accounting,
        })
    }
    /// Attach validated branch metadata when a backend materializes an archive window.
    pub fn with_reasoning(mut self, reasoning: Option<ReasoningSelection>) -> Result<Self> {
        if let Some(choice) = &reasoning {
            choice.validate()?;
            ensure!(
                self.model.as_ref().map(|(p, m)| (p.as_str(), m.as_str()))
                    == Some((choice.provider.as_str(), choice.model.as_str())),
                "window reasoning belongs to another model"
            );
        }
        self.reasoning = reasoning;
        Ok(self)
    }
    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }
    pub fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }
    pub fn messages(&self) -> Vec<SessionMessage<'_>> {
        self.entries
            .iter()
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
    pub fn model_context(&self) -> Result<&[Message]> {
        ensure!(
            self.context_revision == self.metadata.revision,
            "reload the materialized context after committing a window"
        );
        Ok(&self.context)
    }
    pub fn accounting(&self) -> &BranchAccounting {
        &self.accounting
    }
    pub fn source(&self) -> &SessionContextSource {
        &self.source
    }
    pub fn selected_model(&self) -> Option<(&str, &str)> {
        self.model
            .as_ref()
            .map(|(provider, model)| (provider.as_str(), model.as_str()))
    }
    pub fn unfinished_turn(&self) -> Option<&str> {
        self.unfinished.as_deref()
    }
    pub fn attach_backend_state<T: Any + Send + Sync>(&mut self, state: T) {
        self.backend = Some(Arc::new(state));
    }
    pub fn backend_state<T: Any>(&self) -> Option<&T> {
        self.backend.as_ref()?.downcast_ref()
    }
    pub fn run_token(&self) -> Option<&str> {
        self.run_token.as_deref()
    }
    pub fn attach_run_token(&mut self, token: Option<String>) {
        self.run_token = token;
    }
    /// Fold only a successfully persisted batch. Existing archive identities stay immutable.
    pub fn apply_committed(
        &mut self,
        metadata: SessionMetadata,
        entries: Vec<SessionEntry>,
        model: Option<(String, String)>,
        unfinished: Option<String>,
        accounting: BranchAccounting,
    ) -> Result<()> {
        ensure!(
            metadata.id == self.metadata.id
                && metadata.cwd == self.metadata.cwd
                && metadata.revision == self.metadata.revision + 1,
            "invalid window commit identity or revision"
        );
        let mut reasoning = self.reasoning.clone();
        let mut epoch = self.model.clone();
        for entry in &entries {
            match &entry.data {
                SessionEntryData::ModelChanged { provider, model } => {
                    reasoning = None;
                    epoch = Some((provider.clone(), model.clone()));
                }
                SessionEntryData::Extension { namespace, data }
                    if namespace == REASONING_NAMESPACE =>
                {
                    let choice: ReasoningSelection = serde_json::from_value(data.clone())?;
                    choice.validate()?;
                    ensure!(
                        epoch.as_ref().map(|(p, m)| (p.as_str(), m.as_str()))
                            == Some((choice.provider.as_str(), choice.model.as_str())),
                        "window reasoning belongs to another model"
                    );
                    reasoning = Some(choice);
                }
                _ => {}
            }
        }
        ensure!(
            reasoning.as_ref().is_none_or(|choice| model
                .as_ref()
                .map(|(p, m)| (p.as_str(), m.as_str()))
                == Some((choice.provider.as_str(), choice.model.as_str()))),
            "window reasoning belongs to another model"
        );
        self.reasoning = reasoning;
        self.source.recorded_messages = self.source.recorded_messages.saturating_add(
            entries
                .iter()
                .filter(|entry| entry.data.as_message().is_some())
                .count(),
        );
        self.source.revision = metadata.revision;
        self.source.leaf_id = metadata.leaf_id.clone();
        self.metadata = metadata;
        self.entries.extend(entries);
        self.model = model;
        self.unfinished = unfinished;
        self.accounting = accounting;
        Ok(())
    }
}
/// A backend may retain the full snapshot path for small/legacy sessions and return a
/// window when archive growth would exceed materialization budgets.
pub enum RunSession {
    Full(Box<Session>),
    Window(Box<SessionWindow>),
}
impl RunSession {
    pub fn project_context(
        &self,
        repository: &dyn SessionRepository,
        active_turn: Option<&str>,
    ) -> Result<Option<OwnedSessionContext>> {
        match self {
            Self::Full(session) => {
                let branch = session.selected_branch();
                let projection = match active_turn {
                    Some(turn) => branch.context_for_active_turn(turn)?,
                    None => branch.model_context()?,
                };
                Ok(Some(projection.into_owned()))
            }
            Self::Window(window) => repository.project_window_context(window, active_turn),
        }
    }

    pub fn metadata(&self) -> &SessionMetadata {
        match self {
            Self::Full(session) => session.metadata(),
            Self::Window(window) => window.metadata(),
        }
    }
    pub fn selected_model(&self) -> Option<(&str, &str)> {
        match self {
            Self::Full(session) => session.selected_model(),
            Self::Window(window) => window.selected_model(),
        }
    }
    pub fn unfinished_turn(&self) -> Option<&str> {
        match self {
            Self::Full(session) => session.unfinished_turn(),
            Self::Window(window) => window.unfinished_turn(),
        }
    }
    pub fn messages(&self) -> Vec<SessionMessage<'_>> {
        match self {
            Self::Full(session) => session.messages(),
            Self::Window(window) => window.messages(),
        }
    }
    pub fn entry(&self, id: &str) -> Option<&SessionEntry> {
        match self {
            Self::Full(session) => session.entry(id),
            Self::Window(window) => window.entries.iter().find(|entry| entry.id == id),
        }
    }
    pub fn source_and_context(&self) -> Result<(SessionContextSource, Vec<Message>)> {
        match self {
            Self::Full(session) => {
                let context = session.selected_branch().model_context()?;
                Ok((context.source, context.messages))
            }
            Self::Window(window) => Ok((window.source.clone(), window.model_context()?.to_vec())),
        }
    }
    pub fn acquire(&mut self, repository: &dyn SessionRepository) -> Result<SessionRunLease> {
        match self {
            Self::Full(session) => repository.acquire_run(session),
            Self::Window(window) => repository.acquire_window(window),
        }
    }
    pub fn append(
        &mut self,
        repository: &dyn SessionRepository,
        data: Vec<SessionEntryData>,
    ) -> Result<()> {
        match self {
            Self::Full(session) => repository.append_entries(session, data),
            Self::Window(window) => repository.append_window(window, data),
        }
    }
    pub fn accounting(&self) -> Result<BranchAccounting> {
        match self {
            Self::Full(session) => session.selected_branch().accounting(),
            Self::Window(window) => Ok(window.accounting.clone()),
        }
    }
    pub fn clear_run_token(&mut self) {
        match self {
            Self::Full(session) => session.attach_run_token(None),
            Self::Window(window) => window.attach_run_token(None),
        }
    }
    pub fn path(&self) -> Option<PathBuf> {
        match self {
            Self::Full(session) => session.path().map(std::path::Path::to_path_buf),
            Self::Window(_) => None,
        }
    }
}
