use std::path::{Path, PathBuf};

use crate::types::ChatMessage;

/// Metadata shared by all records in a conversation session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    pub version: u32,
    pub id: String,
    pub cwd: PathBuf,
    pub created_at: u64,
    pub updated_at: u64,
    pub title: Option<String>,
}

/// A model-protocol message plus session-local identity and creation time.
#[derive(Debug, Clone)]
pub struct SessionMessage {
    pub id: String,
    pub created_at: u64,
    pub message: ChatMessage,
}

/// An opened session. Mutations should go through its repository.
#[derive(Debug, Clone)]
pub struct Session {
    pub(crate) metadata: SessionMetadata,
    pub(crate) messages: Vec<SessionMessage>,
    pub(crate) path: Option<PathBuf>,
    pub(crate) expected_file_len: u64,
    pub(crate) valid_file_len: u64,
    pub(crate) needs_separator: bool,
}

impl Session {
    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    pub fn messages(&self) -> &[SessionMessage] {
        &self.messages
    }

    pub fn chat_messages(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .map(|entry| entry.message.clone())
            .collect()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// Lightweight data used by session listings and selectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub cwd: PathBuf,
    pub created_at: u64,
    pub updated_at: u64,
    pub title: Option<String>,
    pub message_count: usize,
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
            message_count: session.messages.len(),
            path: session.path.clone(),
        }
    }
}
