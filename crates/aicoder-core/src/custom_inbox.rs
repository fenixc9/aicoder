//! Bounded ephemeral host data for explicit next-turn delivery. Never serialized authority.
use crate::{CustomMessage, RunControlError};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[derive(Debug, Clone)]
pub(crate) struct CustomQueueOrigin {
    pub id: u64,
    pub run_id: crate::RunId,
    pub input_id: u64,
}
#[derive(Debug, Clone)]
pub struct PendingCustomMessage {
    pub id: u64,
    pub message: CustomMessage,
}
struct Entry {
    origin: CustomQueueOrigin,
    id: u64,
    session: String,
    message: CustomMessage,
    bytes: usize,
    claim: Option<uuid::Uuid>,
}
struct State {
    next: u64,
    bytes: usize,
    entries: VecDeque<Entry>,
}
pub struct CustomInbox {
    workspace: PathBuf,
    state: Mutex<State>,
}
impl CustomInbox {
    pub(crate) fn at_workspace(workspace: PathBuf) -> Self {
        Self {
            workspace,
            state: Mutex::new(State {
                next: 1,
                bytes: 0,
                entries: VecDeque::new(),
            }),
        }
    }
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
    pub fn pending(&self, session: &str) -> Vec<PendingCustomMessage> {
        self.state
            .lock()
            .expect("custom inbox poisoned")
            .entries
            .iter()
            .filter(|e| e.session == session)
            .map(|e| PendingCustomMessage {
                id: e.id,
                message: e.message.clone(),
            })
            .collect()
    }
    pub fn discard(&self, session: &str, id: u64) -> Result<bool, RunControlError> {
        let mut state = self.state.lock().expect("custom inbox poisoned");
        let Some(index) = state
            .entries
            .iter()
            .position(|e| e.session == session && e.id == id)
        else {
            return Ok(false);
        };
        if state.entries[index].claim.is_some() {
            return Err(RunControlError::CustomBusy);
        }
        let entry = state.entries.remove(index).expect("inbox entry");
        state.bytes -= entry.bytes;
        Ok(true)
    }
    pub(crate) fn push(
        &self,
        session: &str,
        message: CustomMessage,
        run_id: crate::RunId,
        input_id: u64,
    ) -> anyhow::Result<u64> {
        message.validate()?;
        let bytes = crate::extensions::size(&message, crate::MAX_CUSTOM_MESSAGE_BYTES)?;
        let mut state = self.state.lock().expect("custom inbox poisoned");
        if state.entries.len() >= 32 || bytes > (4 * 1024 * 1024usize).saturating_sub(state.bytes) {
            return Err(RunControlError::QueueFull.into());
        }
        let id = state.next;
        state.next = id.checked_add(1).ok_or(RunControlError::QueueFull)?;
        state.bytes += bytes;
        state.entries.push_back(Entry {
            origin: CustomQueueOrigin {
                id,
                run_id,
                input_id,
            },
            id,
            session: session.into(),
            message,
            bytes,
            claim: None,
        });
        Ok(id)
    }
    pub(crate) fn claim(self: &Arc<Self>, session: &str) -> anyhow::Result<CustomClaim> {
        let mut state = self.state.lock().expect("custom inbox poisoned");
        anyhow::ensure!(
            !state
                .entries
                .iter()
                .any(|e| e.session == session && e.claim.is_some()),
            "custom inbox already claimed for this session"
        );
        let token = uuid::Uuid::new_v4();
        let mut messages = Vec::new();
        let mut origins = Vec::new();
        for entry in state.entries.iter_mut().filter(|e| e.session == session) {
            entry.claim = Some(token);
            messages.push(entry.message.clone());
            origins.push(entry.origin.clone());
        }
        Ok(CustomClaim {
            inbox: self.clone(),
            token,
            messages,
            origins,
            committed: false,
        })
    }
}
pub(crate) struct CustomClaim {
    inbox: Arc<CustomInbox>,
    token: uuid::Uuid,
    pub messages: Vec<CustomMessage>,
    pub origins: Vec<CustomQueueOrigin>,
    committed: bool,
}
impl CustomClaim {
    pub fn commit(mut self) {
        let mut state = self.inbox.state.lock().expect("custom inbox poisoned");
        let mut retained = VecDeque::new();
        while let Some(entry) = state.entries.pop_front() {
            if entry.claim == Some(self.token) {
                state.bytes -= entry.bytes;
            } else {
                retained.push_back(entry);
            }
        }
        state.entries = retained;
        self.committed = true;
    }
}
impl Drop for CustomClaim {
    fn drop(&mut self) {
        if !self.committed {
            let mut state = self.inbox.state.lock().expect("custom inbox poisoned");
            for entry in &mut state.entries {
                if entry.claim == Some(self.token) {
                    entry.claim = None;
                }
            }
        }
    }
}
