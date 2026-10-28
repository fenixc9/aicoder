use super::{Session, SessionEntryData, SessionInfo, SessionTransaction};
use crate::model::Message;
use anyhow::Result;
use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
};
/// Dropping the owned lease releases single-run ownership, including on task cancellation.
pub type SessionRunLease = Box<dyn Send>;
#[derive(Debug, Clone)]
pub struct SessionConflict {
    pub id: String,
}
impl fmt::Display for SessionConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Session {} was modified by another process; reopen it before appending",
            self.id
        )
    }
}
impl Error for SessionConflict {}
/// Object-safe storage and transaction boundary, shared by all hosts.
pub trait SessionRepository: Send + Sync {
    fn create(&self, cwd: &Path) -> Result<Session>;
    fn open(&self, id: &str) -> Result<Session>;
    fn list(&self, cwd: &Path) -> Result<Vec<SessionInfo>>;
    fn commit(&self, session: &mut Session, transaction: SessionTransaction) -> Result<()>;
    fn acquire_run(&self, session: &mut Session) -> Result<SessionRunLease>;
    fn delete(&self, id: &str) -> Result<()>;
    fn migrate(&self, session: &mut Session) -> Result<Option<PathBuf>>;
    fn append_entries(&self, session: &mut Session, data: Vec<SessionEntryData>) -> Result<()> {
        let transaction = session.transaction(data)?;
        self.commit(session, transaction)
    }
    fn append(&self, session: &mut Session, message: Message) -> Result<()> {
        self.append_entries(session, vec![SessionEntryData::Message { message }])
    }
    fn append_all<I: IntoIterator<Item = Message>>(
        &self,
        session: &mut Session,
        messages: I,
    ) -> Result<()>
    where
        Self: Sized,
    {
        self.append_entries(
            session,
            messages
                .into_iter()
                .map(|message| SessionEntryData::Message { message })
                .collect(),
        )
    }
    fn checkout(&self, session: &mut Session, leaf: Option<&str>) -> Result<()> {
        self.commit(
            session,
            SessionTransaction {
                entries: Vec::new(),
                leaf_id: leaf.map(str::to_owned),
            },
        )
    }
    fn most_recent(&self, cwd: &Path) -> Result<Option<SessionInfo>> {
        Ok(self.list(cwd)?.into_iter().next())
    }
}
