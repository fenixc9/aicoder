//! A one-use append preview; its owned IDs are committed verbatim after model admission.
use super::*;
use anyhow::{Result, ensure};
pub struct SessionAppendPlan {
    source: SessionMetadata,
    transaction: SessionTransaction,
    projection: OwnedSessionContext,
}
impl SessionAppendPlan {
    pub fn new(
        source: SessionMetadata,
        transaction: SessionTransaction,
        projection: OwnedSessionContext,
    ) -> Result<Self> {
        ensure!(
            projection.source.session_id == source.id
                && projection.source.revision
                    == source
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("append revision exhausted"))?
                && projection.source.leaf_id == transaction.leaf_id,
            "append projection does not match source/transaction"
        );
        ensure!(
            projection.messages.len() == projection.entry_ids.len(),
            "append projection origin count differs"
        );
        Ok(Self {
            source,
            transaction,
            projection,
        })
    }
    pub fn source(&self) -> &SessionMetadata {
        &self.source
    }
    pub fn projection(&self) -> &OwnedSessionContext {
        &self.projection
    }
    pub fn transaction(&self) -> &SessionTransaction {
        &self.transaction
    }
    pub fn validate_source(&self, source: &SessionMetadata) -> Result<()> {
        ensure!(
            self.source.id == source.id
                && self.source.cwd == source.cwd
                && self.source.revision == source.revision
                && self.source.leaf_id == source.leaf_id,
            "stale or foreign prepared append"
        );
        Ok(())
    }
    pub fn into_transaction(self) -> SessionTransaction {
        self.transaction
    }
    pub fn full(
        session: &Session,
        transaction: SessionTransaction,
        active_turn: Option<&str>,
    ) -> Result<Self> {
        let mut candidate = session.clone();
        candidate.apply_transaction(
            transaction.clone(),
            session
                .metadata()
                .revision
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("append revision exhausted"))?,
            super::now_millis()?,
        )?;
        let branch = candidate.selected_branch();
        let projected = match active_turn {
            Some(turn) => branch.context_for_active_turn(turn)?,
            None => branch.model_context()?,
        };
        Self::new(
            session.metadata().clone(),
            transaction,
            projected.into_owned(),
        )
    }
}
impl RunSession {
    pub fn prepare_append(
        &self,
        repository: &dyn SessionRepository,
        commit: &crate::extensions::BoundaryCommit,
        active_turn: Option<&str>,
    ) -> Result<SessionAppendPlan> {
        match self {
            Self::Full(s) => {
                SessionAppendPlan::full(s, commit.session_transaction(s, active_turn)?, active_turn)
            }
            Self::Window(w) => repository.prepare_window_boundary(w, commit, active_turn),
        }
    }
    pub fn commit_append(
        &mut self,
        repository: &dyn SessionRepository,
        plan: SessionAppendPlan,
    ) -> Result<()> {
        plan.validate_source(self.metadata())?;
        match self {
            Self::Full(s) => repository.commit(s, plan.into_transaction()),
            Self::Window(w) => repository.commit_window_append(w, plan),
        }
    }
}
