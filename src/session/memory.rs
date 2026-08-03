use std::{
    collections::HashMap,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::types::{ChatMessage, Role};

use super::{Session, SessionInfo, SessionMessage, SessionMetadata, SessionRepository};

const SESSION_VERSION: u32 = 1;

/// In-process repository intended for tests and ephemeral frontends.
#[derive(Debug, Default)]
pub struct MemorySessionRepository {
    sessions: Mutex<HashMap<String, Session>>,
}

impl MemorySessionRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

fn now_millis() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before the Unix epoch")?
        .as_millis()
        .try_into()
        .context("Current timestamp does not fit in u64")
}

fn title_from_message(message: &ChatMessage) -> Option<String> {
    if message.role != Role::User {
        return None;
    }
    let title = message
        .content
        .as_deref()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        return None;
    }
    Some(title.chars().take(80).collect())
}

fn canonical_cwd(cwd: &Path) -> Result<std::path::PathBuf> {
    cwd.canonicalize()
        .with_context(|| format!("Failed to resolve workspace {}", cwd.display()))
}

impl SessionRepository for MemorySessionRepository {
    fn create(&self, cwd: &Path) -> Result<Session> {
        let timestamp = now_millis()?;
        let id = Uuid::new_v4().to_string();
        let session = Session {
            metadata: SessionMetadata {
                version: SESSION_VERSION,
                id: id.clone(),
                cwd: canonical_cwd(cwd)?,
                created_at: timestamp,
                updated_at: timestamp,
                title: None,
            },
            messages: Vec::new(),
            path: None,
            expected_file_len: 0,
            valid_file_len: 0,
            needs_separator: false,
        };
        self.sessions
            .lock()
            .expect("memory session repository lock poisoned")
            .insert(id, session.clone());
        Ok(session)
    }

    fn open(&self, id: &str) -> Result<Session> {
        self.sessions
            .lock()
            .expect("memory session repository lock poisoned")
            .get(id)
            .cloned()
            .with_context(|| format!("Session {id} not found"))
    }

    fn list(&self, cwd: &Path) -> Result<Vec<SessionInfo>> {
        let cwd = canonical_cwd(cwd)?;
        let mut sessions = self
            .sessions
            .lock()
            .expect("memory session repository lock poisoned")
            .values()
            .filter(|session| session.metadata.cwd == cwd)
            .map(SessionInfo::from)
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(sessions)
    }

    fn append(&self, session: &mut Session, message: ChatMessage) -> Result<()> {
        if message.role == Role::System {
            bail!("System messages are runtime context and cannot be stored in a session");
        }
        let timestamp = now_millis()?;
        session.messages.push(SessionMessage {
            id: Uuid::new_v4().to_string(),
            created_at: timestamp,
            message,
        });
        session.metadata.updated_at = timestamp;
        if session.metadata.title.is_none() {
            session.metadata.title = title_from_message(&session.messages.last().unwrap().message);
        }
        self.sessions
            .lock()
            .expect("memory session repository lock poisoned")
            .insert(session.metadata.id.clone(), session.clone());
        Ok(())
    }

    fn delete(&self, id: &str) -> Result<()> {
        if self
            .sessions
            .lock()
            .expect("memory session repository lock poisoned")
            .remove(id)
            .is_none()
        {
            bail!("Session {id} not found");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn user_message(content: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: Some(content.to_string()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn memory_repository_round_trips_and_lists_sessions() {
        let cwd = tempdir().unwrap();
        let repository = MemorySessionRepository::new();
        let mut session = repository.create(cwd.path()).unwrap();
        repository
            .append(&mut session, user_message("  inspect   this project  "))
            .unwrap();

        let reopened = repository.open(&session.metadata().id).unwrap();
        assert_eq!(reopened.messages().len(), 1);
        assert_eq!(
            reopened.metadata().title.as_deref(),
            Some("inspect this project")
        );
        assert_eq!(repository.list(cwd.path()).unwrap().len(), 1);

        repository.delete(&session.metadata().id).unwrap();
        assert!(repository.list(cwd.path()).unwrap().is_empty());
    }

    #[test]
    fn memory_repository_rejects_system_messages() {
        let cwd = tempdir().unwrap();
        let repository = MemorySessionRepository::new();
        let mut session = repository.create(cwd.path()).unwrap();
        let mut message = user_message("system");
        message.role = Role::System;

        let error = repository.append(&mut session, message).unwrap_err();
        assert!(error.to_string().contains("System messages"));
    }
}
