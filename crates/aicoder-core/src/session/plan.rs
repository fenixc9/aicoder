//! Read-only navigation preparation. Owned entries and backend authority never restore from JSON.
use super::*;
use anyhow::{Result, ensure};
/// A proposal over a pinned source. Its preview is not committed session history.
pub struct SessionNavigationPlan {
    source: SessionMetadata,
    action: ArchiveNavigation,
    transaction: Option<SessionTransaction>,
    preview: RunSession,
}
impl SessionNavigationPlan {
    /// Backend boundary: preview records must be real; planned metadata entries may select their ancestry.
    pub fn new(
        source: SessionMetadata,
        action: ArchiveNavigation,
        transaction: Option<SessionTransaction>,
        mut preview: RunSession,
    ) -> Result<Self> {
        ensure!(
            source.id == preview.metadata().id && source.cwd == preview.metadata().cwd,
            "navigation preview belongs to another session/workspace"
        );
        if let Some(transaction) = &transaction {
            ensure!(
                source.revision.checked_add(1) == Some(preview.metadata().revision)
                    && transaction.leaf_id == preview.metadata().leaf_id,
                "navigation preview does not match its transaction"
            );
        } else {
            ensure!(
                source.revision == preview.metadata().revision
                    && source.leaf_id == preview.metadata().leaf_id,
                "no-op navigation changed its preview"
            );
        }
        preview.clear_run_token();
        Ok(Self {
            source,
            action,
            transaction,
            preview,
        })
    }
    pub fn source(&self) -> &SessionMetadata {
        &self.source
    }
    pub fn target(&self) -> &SessionMetadata {
        self.preview.metadata()
    }
    pub fn action(&self) -> &ArchiveNavigation {
        &self.action
    }
    pub fn has_changes(&self) -> bool {
        self.transaction.is_some()
    }
    pub(crate) fn preview(&self) -> &RunSession {
        &self.preview
    }
    pub fn into_transaction(self) -> Option<SessionTransaction> {
        self.transaction
    }
    pub fn validate_source(&self, metadata: &SessionMetadata) -> Result<()> {
        ensure!(
            self.source.id == metadata.id
                && self.source.cwd == metadata.cwd
                && self.source.revision == metadata.revision
                && self.source.leaf_id == metadata.leaf_id,
            "stale or foreign prepared navigation"
        );
        Ok(())
    }
    pub fn full(session: &Session, action: ArchiveNavigation) -> Result<Self> {
        let (action, transaction) = prepare_transaction(session, action)?;
        let mut preview = session.clone();
        if let Some(transaction) = &transaction {
            preview.apply_transaction(
                transaction.clone(),
                session
                    .metadata()
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("navigation revision exhausted"))?,
                super::now_millis()?,
            )?;
        }
        preview.selected_branch().model_context()?;
        Self::new(
            session.metadata().clone(),
            action,
            transaction,
            RunSession::Full(Box::new(preview)),
        )
    }
}
/// Resolve names and freeze entry IDs before guards or any persistent effect.
pub fn prepare_transaction(
    session: &Session,
    action: ArchiveNavigation,
) -> Result<(ArchiveNavigation, Option<SessionTransaction>)> {
    let mut parent = session.metadata().leaf_id.clone();
    let mut data = None;
    let action = match action {
        ArchiveNavigation::Rename { title } => {
            super::navigation::name(&title)?;
            data = Some(
                NavigationRecord::Rename {
                    title: title.clone(),
                }
                .data(),
            );
            ArchiveNavigation::Rename { title }
        }
        ArchiveNavigation::Label { entry_id, label } => {
            let entry_id = session.resolve_entry(&entry_id)?.to_owned();
            if let Some(label) = &label {
                super::navigation::name(label)?;
            }
            data = Some(
                NavigationRecord::Label {
                    entry_id: entry_id.clone(),
                    label: label.clone(),
                }
                .data(),
            );
            ArchiveNavigation::Label { entry_id, label }
        }
        ArchiveNavigation::Checkout { target } => {
            parent = target
                .as_deref()
                .map(|v| session.resolve_entry(v).map(str::to_owned))
                .transpose()?;
            ArchiveNavigation::Checkout {
                target: parent.clone(),
            }
        }
        ArchiveNavigation::Branch { source, name } => {
            if let Some(name) = &name {
                super::navigation::name(name)?;
            }
            parent = match source {
                BranchSource::Current => parent,
                BranchSource::Root => None,
                BranchSource::Entry(target) => Some(session.resolve_entry(&target)?.to_owned()),
            };
            session.branch(parent.as_deref())?.model_context()?;
            data = Some(NavigationRecord::Branch { name: name.clone() }.data());
            ArchiveNavigation::Branch {
                source: parent
                    .clone()
                    .map_or(BranchSource::Root, BranchSource::Entry),
                name,
            }
        }
        ArchiveNavigation::Recover => {
            session.selected_branch().model_context()?;
            if let Some(turn) = session.unfinished_turn() {
                data = Some(SessionEntryData::TurnEnd {
                    turn_id: turn.into(),
                    record: Box::new(TurnRecord::recovered()),
                });
            } else {
                return Ok((ArchiveNavigation::Recover, None));
            }
            ArchiveNavigation::Recover
        }
    };
    let entries = if let Some(data) = data {
        let id = uuid::Uuid::new_v4().to_string();
        let entry = SessionEntry {
            id: id.clone(),
            parent_id: parent,
            created_at: super::now_millis()?,
            data,
        };
        parent = Some(id);
        vec![entry]
    } else {
        Vec::new()
    };
    let transaction = SessionTransaction {
        entries,
        leaf_id: parent,
    };
    session.validate_transaction(&transaction)?;
    Ok((action, Some(transaction)))
}
/// Holds the repository lease across asynchronous preparation/guards and the exact commit.
pub struct PreparedSessionNavigation<'a> {
    repository: &'a dyn SessionRepository,
    state: RunSession,
    plan: SessionNavigationPlan,
    _lease: SessionRunLease,
}
impl<'a> PreparedSessionNavigation<'a> {
    pub(crate) fn new(
        repository: &'a dyn SessionRepository,
        mut state: RunSession,
        action: ArchiveNavigation,
    ) -> Result<Self> {
        let lease = state.acquire(repository)?;
        let plan = match &state {
            RunSession::Full(s) => SessionNavigationPlan::full(s, action)?,
            RunSession::Window(w) => repository.prepare_window_navigation(w, action)?,
        };
        plan.validate_source(state.metadata())?;
        Ok(Self {
            repository,
            state,
            plan,
            _lease: lease,
        })
    }
    pub fn plan(&self) -> &SessionNavigationPlan {
        &self.plan
    }
    pub fn commit(self) -> Result<RunSession> {
        let (state, lease) = self.commit_owned()?;
        drop(lease);
        Ok(state)
    }
    pub(crate) fn commit_owned(mut self) -> Result<(RunSession, SessionRunLease)> {
        self.plan.validate_source(self.state.metadata())?;
        match &mut self.state {
            RunSession::Full(session) => {
                if let Some(transaction) = self.plan.into_transaction() {
                    self.repository.commit(session, transaction)?;
                }
            }
            RunSession::Window(window) => self
                .repository
                .commit_window_navigation(window, self.plan)?,
        }
        self.state.clear_run_token();
        Ok((self.state, self._lease))
    }
}
