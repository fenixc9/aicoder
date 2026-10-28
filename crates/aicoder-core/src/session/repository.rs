use std::path::Path;

use anyhow::Result;

use crate::types::ChatMessage;

use super::{Session, SessionInfo};

/// Persistence boundary for conversation sessions.
pub trait SessionRepository {
    fn create(&self, cwd: &Path) -> Result<Session>;
    fn open(&self, id: &str) -> Result<Session>;
    fn list(&self, cwd: &Path) -> Result<Vec<SessionInfo>>;
    fn append(&self, session: &mut Session, message: ChatMessage) -> Result<()>;
    fn delete(&self, id: &str) -> Result<()>;

    fn append_all<I>(&self, session: &mut Session, messages: I) -> Result<()>
    where
        I: IntoIterator<Item = ChatMessage>,
        Self: Sized,
    {
        for message in messages {
            self.append(session, message)?;
        }
        Ok(())
    }

    fn most_recent(&self, cwd: &Path) -> Result<Option<SessionInfo>> {
        Ok(self.list(cwd)?.into_iter().next())
    }
}
