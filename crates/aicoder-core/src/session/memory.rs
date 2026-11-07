use super::*;
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[derive(Debug, Default)]
struct State {
    sessions: HashMap<String, Session>,
    runs: HashMap<String, String>,
}
#[derive(Debug, Default, Clone)]
pub struct MemorySessionRepository {
    state: Arc<Mutex<State>>,
}
impl MemorySessionRepository {
    pub fn new() -> Self {
        Self::default()
    }
}
struct Lease {
    state: Arc<Mutex<State>>,
    id: String,
    token: String,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.runs.get(&self.id) == Some(&self.token) {
            state.runs.remove(&self.id);
        }
    }
}
impl SessionRepository for MemorySessionRepository {
    fn publish_fork(
        &self,
        source: &RunSession,
        plan: SessionForkPlan,
        context: &crate::TurnExecutionContext,
    ) -> Result<PublishedSessionFork> {
        plan.validate_source(source)?;
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let mut state = self.state.lock().expect("memory repository poisoned");
        let actual = state
            .sessions
            .get(&plan.source().id)
            .context("fork source missing")?;
        ensure!(actual.metadata() == plan.source(), "fork source changed");
        let token = match source {
            RunSession::Full(s) => s.run_token(),
            RunSession::Window(w) => w.run_token(),
        };
        ensure!(
            state
                .runs
                .get(&plan.source().id)
                .is_some_and(|t| Some(t.as_str()) == token),
            "fork requires owned source"
        );
        let mut target = plan.snapshot().clone();
        target.clear_backend_state();
        target.attach_run_token(None);
        let id = target.metadata().id.clone();
        ensure!(
            !state.sessions.contains_key(&id),
            "fork target already exists"
        );
        let token = uuid::Uuid::new_v4().to_string();
        state.sessions.insert(id.clone(), target.clone());
        state.runs.insert(id.clone(), token.clone());
        target.attach_run_token(Some(token.clone()));
        Ok(PublishedSessionFork {
            session: RunSession::Full(Box::new(target)),
            lease: Box::new(Lease {
                state: self.state.clone(),
                id,
                token,
            }),
        })
    }

    fn create(&self, cwd: &Path) -> Result<Session> {
        let timestamp = now_millis()?;
        let id = uuid::Uuid::new_v4().to_string();
        let session = Session::empty(SessionMetadata {
            version: SESSION_VERSION,
            id: id.clone(),
            cwd: cwd.canonicalize()?,
            created_at: timestamp,
            updated_at: timestamp,
            title: None,
            leaf_id: None,
            revision: 0,
        });
        self.state
            .lock()
            .expect("memory repository poisoned")
            .sessions
            .insert(id, session.clone());
        Ok(session)
    }
    fn open(&self, id: &str) -> Result<Session> {
        let mut session = self
            .state
            .lock()
            .expect("memory repository poisoned")
            .sessions
            .get(id)
            .cloned()
            .context("session not found")?;
        session.run_token = None;
        Ok(session)
    }
    fn list(&self, cwd: &Path) -> Result<Vec<SessionInfo>> {
        let cwd = cwd.canonicalize()?;
        let mut sessions = self
            .state
            .lock()
            .expect("memory repository poisoned")
            .sessions
            .values()
            .filter(|session| session.metadata.cwd == cwd)
            .map(SessionInfo::from)
            .collect::<Vec<_>>();
        sort_sessions(&mut sessions);
        Ok(sessions)
    }
    fn commit(&self, session: &mut Session, transaction: SessionTransaction) -> Result<()> {
        session.validate(&transaction)?;
        let mut state = self.state.lock().expect("memory repository poisoned");
        let stored = state
            .sessions
            .get(&session.metadata.id)
            .context("session not found")?;
        if stored.metadata.revision != session.metadata.revision {
            return Err(SessionConflict {
                id: session.metadata.id.clone(),
            }
            .into());
        }
        ensure!(
            state
                .runs
                .get(&session.metadata.id)
                .is_none_or(|token| session.run_token.as_ref() == Some(token)),
            "session already has an active run"
        );
        let revision = session
            .metadata
            .revision
            .checked_add(1)
            .context("session revision overflow")?;
        let timestamp = now_millis()?;
        state.sessions.get_mut(&session.metadata.id).unwrap().apply(
            transaction.clone(),
            revision,
            timestamp,
        );
        session.apply(transaction, revision, timestamp);
        Ok(())
    }
    fn acquire_run(&self, session: &mut Session) -> Result<SessionRunLease> {
        let mut state = self.state.lock().expect("memory repository poisoned");
        ensure!(
            state.sessions.contains_key(&session.metadata.id),
            "session not found"
        );
        ensure!(
            !state.runs.contains_key(&session.metadata.id),
            "session already has an active run"
        );
        let token = uuid::Uuid::new_v4().to_string();
        state
            .runs
            .insert(session.metadata.id.clone(), token.clone());
        session.run_token = Some(token.clone());
        Ok(Box::new(Lease {
            state: self.state.clone(),
            id: session.metadata.id.clone(),
            token,
        }))
    }
    fn delete(&self, id: &str) -> Result<()> {
        let mut state = self.state.lock().expect("memory repository poisoned");
        ensure!(
            !state.runs.contains_key(id),
            "session already has an active run"
        );
        state.sessions.remove(id).context("session not found")?;
        Ok(())
    }
    fn migrate(&self, _: &mut Session) -> Result<Option<PathBuf>> {
        Ok(None)
    }
}
