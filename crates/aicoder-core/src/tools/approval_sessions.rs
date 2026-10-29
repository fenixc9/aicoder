//! Host-owned, bounded, ephemeral authorization; serialized snapshots carry no authority.
use super::{ApprovalDecision, ToolCapability, ToolInvocation};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use uuid::Uuid;
#[derive(Debug, Clone, Copy)]
pub struct ApprovalCacheLimits {
    pub max_sessions: usize,
    pub max_grants_per_session: usize,
}
impl Default for ApprovalCacheLimits {
    fn default() -> Self {
        Self {
            max_sessions: 32,
            max_grants_per_session: 64,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalSessionInfo {
    pub scope_id: String,
    pub session_id: String,
    pub workspace: PathBuf,
    pub epoch: u64,
    pub closed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalGrant {
    pub id: String,
    pub tool: String,
    pub capability: ToolCapability,
    pub registry_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalSnapshot {
    pub session: ApprovalSessionInfo,
    pub grants: Vec<ApprovalGrant>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalError {
    InvalidScope,
    InvalidWorkspace,
    Closed,
    Revoked,
    LimitExceeded,
    SessionUnavailable,
}
impl std::fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidScope => "Invalid approval scope or tool identity",
            Self::InvalidWorkspace => "Approval workspace identity does not match",
            Self::Closed => "Approval session is closed",
            Self::Revoked => "Approval was revoked while awaiting a decision",
            Self::LimitExceeded => "Approval cache/grant limit exceeded",
            Self::SessionUnavailable => "Session authorization is unavailable for this request",
        })
    }
}
impl std::error::Error for ApprovalError {}
impl ApprovalError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Revoked | Self::Closed => "approval_revoked",
            Self::LimitExceeded => "approval_limit",
            Self::SessionUnavailable => "approval_invalid_decision",
            _ => "approval_scope_mismatch",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Workspace {
    path: PathBuf,
    #[cfg(unix)]
    file_id: (u64, u64),
}
impl Workspace {
    fn new(path: &Path) -> Result<Self, ApprovalError> {
        let path = path
            .canonicalize()
            .map_err(|_| ApprovalError::InvalidWorkspace)?;
        if path.to_str().is_none() || path.as_os_str().len() > 4096 {
            return Err(ApprovalError::InvalidWorkspace);
        }
        let metadata = std::fs::metadata(&path).map_err(|_| ApprovalError::InvalidWorkspace)?;
        if !metadata.is_dir() {
            return Err(ApprovalError::InvalidWorkspace);
        }
        Ok(Self {
            path,
            #[cfg(unix)]
            file_id: {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            },
        })
    }
}
struct SessionState {
    epoch: u64,
    closed: bool,
    grants: Vec<ApprovalGrant>,
}
struct SessionInner {
    id: String,
    session_id: String,
    workspace: Workspace,
    limit: usize,
    state: Mutex<SessionState>,
    changed: tokio::sync::Notify,
}
#[derive(Clone)]
pub struct ApprovalSession(Arc<SessionInner>);
impl std::fmt::Debug for ApprovalSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalSession")
            .field("scope_id", &self.0.id)
            .finish_non_exhaustive()
    }
}
impl ApprovalSession {
    fn new(session_id: &str, workspace: Workspace, limit: usize) -> Self {
        Self(Arc::new(SessionInner {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            workspace,
            limit,
            changed: tokio::sync::Notify::new(),
            state: Mutex::new(SessionState {
                epoch: 0,
                closed: false,
                grants: Vec::new(),
            }),
        }))
    }
    fn info(&self, state: &SessionState) -> ApprovalSessionInfo {
        ApprovalSessionInfo {
            scope_id: self.0.id.clone(),
            session_id: self.0.session_id.clone(),
            workspace: self.0.workspace.path.clone(),
            epoch: state.epoch,
            closed: state.closed,
        }
    }
    pub fn snapshot(&self) -> ApprovalSnapshot {
        let state = self.0.state.lock().expect("approval session poisoned");
        ApprovalSnapshot {
            session: self.info(&state),
            grants: state.grants.clone(),
        }
    }
    pub fn revoke(&self, id: &str) -> bool {
        let mut state = self.0.state.lock().expect("approval session poisoned");
        let before = state.grants.len();
        state.grants.retain(|grant| grant.id != id);
        if before != state.grants.len() {
            bump(&mut state);
            drop(state);
            self.0.changed.notify_waiters();
            true
        } else {
            false
        }
    }
    pub fn revoke_all(&self) {
        let mut state = self.0.state.lock().expect("approval session poisoned");
        state.grants.clear();
        bump(&mut state);
        drop(state);
        self.0.changed.notify_waiters();
    }
    fn close(&self) {
        let mut state = self.0.state.lock().expect("approval session poisoned");
        state.closed = true;
        state.grants.clear();
        bump(&mut state);
        drop(state);
        self.0.changed.notify_waiters();
    }
    pub(crate) fn matches(&self, workspace: &Path, session_id: &str) -> Result<(), ApprovalError> {
        if self.0.session_id != session_id || self.0.workspace != Workspace::new(workspace)? {
            return Err(ApprovalError::InvalidWorkspace);
        }
        if self
            .0
            .state
            .lock()
            .expect("approval session poisoned")
            .closed
        {
            return Err(ApprovalError::Closed);
        }
        Ok(())
    }
    pub(crate) fn ticket(&self, workspace: &Path) -> Result<ApprovalTicket, ApprovalError> {
        self.matches(workspace, &self.0.session_id)?;
        let state = self.0.state.lock().expect("approval session poisoned");
        Ok(ApprovalTicket {
            session: self.clone(),
            info: self.info(&state),
        })
    }
}
fn bump(state: &mut SessionState) {
    match state.epoch.checked_add(1) {
        Some(epoch) => state.epoch = epoch,
        None => state.closed = true,
    }
}
struct Cache {
    limits: ApprovalCacheLimits,
    sessions: Mutex<VecDeque<ApprovalSession>>,
}
/// One instance per trusted host/principal/storage namespace. Clones share authority.
#[derive(Clone)]
pub struct ApprovalSessions(Arc<Cache>);
impl Default for ApprovalSessions {
    fn default() -> Self {
        Self::with_limits(ApprovalCacheLimits::default()).expect("default approval limits")
    }
}
impl std::fmt::Debug for ApprovalSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalSessions")
            .field("limits", &self.0.limits)
            .finish_non_exhaustive()
    }
}
impl ApprovalSessions {
    pub fn with_limits(limits: ApprovalCacheLimits) -> Result<Self, ApprovalError> {
        if !(1..=1024).contains(&limits.max_sessions)
            || !(1..=256).contains(&limits.max_grants_per_session)
        {
            return Err(ApprovalError::LimitExceeded);
        }
        Ok(Self(Arc::new(Cache {
            limits,
            sessions: Mutex::new(VecDeque::new()),
        })))
    }
    pub fn session(
        &self,
        workspace: impl AsRef<Path>,
        session_id: &str,
    ) -> Result<ApprovalSession, ApprovalError> {
        if session_id.is_empty() || session_id.len() > 1024 {
            return Err(ApprovalError::InvalidScope);
        }
        let workspace = Workspace::new(workspace.as_ref())?;
        let mut sessions = self.0.sessions.lock().expect("approval cache poisoned");
        if let Some(index) = sessions
            .iter()
            .position(|s| s.0.session_id == session_id && s.0.workspace == workspace)
        {
            let session = sessions.remove(index).unwrap();
            sessions.push_back(session.clone());
            return Ok(session);
        }
        if sessions.len() == self.0.limits.max_sessions {
            sessions.pop_front().expect("full cache").close();
        }
        let session =
            ApprovalSession::new(session_id, workspace, self.0.limits.max_grants_per_session);
        sessions.push_back(session.clone());
        Ok(session)
    }
    pub fn get(&self, workspace: impl AsRef<Path>, session_id: &str) -> Option<ApprovalSession> {
        let workspace = Workspace::new(workspace.as_ref()).ok()?;
        self.0
            .sessions
            .lock()
            .expect("approval cache poisoned")
            .iter()
            .find(|s| s.0.session_id == session_id && s.0.workspace == workspace)
            .cloned()
    }
    pub fn remove(&self, workspace: impl AsRef<Path>, session_id: &str) -> bool {
        let Ok(workspace) = Workspace::new(workspace.as_ref()) else {
            return false;
        };
        let mut sessions = self.0.sessions.lock().expect("approval cache poisoned");
        if let Some(index) = sessions
            .iter()
            .position(|s| s.0.session_id == session_id && s.0.workspace == workspace)
        {
            sessions.remove(index).unwrap().close();
            true
        } else {
            false
        }
    }
    pub fn clear(&self) {
        for session in self
            .0
            .sessions
            .lock()
            .expect("approval cache poisoned")
            .drain(..)
        {
            session.close();
        }
    }
}
pub(crate) struct ApprovalTicket {
    session: ApprovalSession,
    pub info: ApprovalSessionInfo,
}
impl ApprovalTicket {
    pub fn current(&self) -> Result<(), ApprovalError> {
        self.session
            .matches(&self.info.workspace, &self.info.session_id)?;
        let state = self
            .session
            .0
            .state
            .lock()
            .expect("approval session poisoned");
        if state.epoch != self.info.epoch {
            return Err(ApprovalError::Revoked);
        }
        Ok(())
    }
    pub async fn revoked(&self) -> ApprovalError {
        loop {
            let notified = self.session.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Err(error) = self.current() {
                return error;
            }
            notified.await;
        }
    }
    pub fn lookup(&self, registry: Uuid, invocation: &ToolInvocation) -> Option<String> {
        let state = self
            .session
            .0
            .state
            .lock()
            .expect("approval session poisoned");
        if state.closed || state.epoch != self.info.epoch {
            return None;
        }
        let registry = registry.to_string();
        state
            .grants
            .iter()
            .find(|g| {
                g.registry_id == registry
                    && g.tool == invocation.name
                    && g.capability == invocation.capability
            })
            .map(|g| g.id.clone())
    }
    pub fn apply(
        &self,
        registry: Uuid,
        invocation: &ToolInvocation,
        decision: ApprovalDecision,
    ) -> Result<Option<String>, ApprovalError> {
        self.current()?;
        let mut state = self
            .session
            .0
            .state
            .lock()
            .expect("approval session poisoned");
        if state.closed || state.epoch != self.info.epoch {
            return Err(ApprovalError::Revoked);
        }
        if decision != ApprovalDecision::AllowSession {
            return Ok(None);
        }
        if invocation.name.is_empty() || invocation.name.len() > 1024 {
            return Err(ApprovalError::InvalidScope);
        }
        let registry = registry.to_string();
        if let Some(grant) = state.grants.iter().find(|g| {
            g.registry_id == registry
                && g.tool == invocation.name
                && g.capability == invocation.capability
        }) {
            return Ok(Some(grant.id.clone()));
        }
        if state.grants.len() >= self.session.0.limit {
            return Err(ApprovalError::LimitExceeded);
        }
        let id = Uuid::new_v4().to_string();
        state.grants.push(ApprovalGrant {
            id: id.clone(),
            tool: invocation.name.clone(),
            capability: invocation.capability,
            registry_id: registry,
        });
        Ok(Some(id))
    }
}
