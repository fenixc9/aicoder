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
    /// Default compatibility path. Window-capable backends override without materializing cold payloads.
    fn open_run(&self, id: &str) -> Result<super::RunSession> {
        self.open(id)
            .map(|session| super::RunSession::Full(Box::new(session)))
    }
    fn append_window(
        &self,
        _window: &mut super::SessionWindow,
        _entries: Vec<SessionEntryData>,
    ) -> Result<()> {
        anyhow::bail!("repository does not support window commits")
    }
    fn acquire_window(&self, _window: &mut super::SessionWindow) -> Result<SessionRunLease> {
        anyhow::bail!("repository does not support window ownership")
    }
    fn archive_page(
        &self,
        id: &str,
        limit: usize,
        cursor: Option<&super::ArchiveCursor>,
    ) -> Result<super::ArchivePage> {
        anyhow::ensure!(
            (1..=100).contains(&limit),
            "archive page limit must be 1..100"
        );
        let session = self.open(id)?;
        let offset = if let Some(cursor) = cursor {
            anyhow::ensure!(
                cursor.session_id == id && cursor.revision == session.metadata().revision,
                "stale or foreign archive cursor"
            );
            cursor.offset
        } else {
            0
        };
        anyhow::ensure!(offset <= session.entries().len(), "invalid archive offset");
        let end = offset.saturating_add(limit).min(session.entries().len());
        Ok(super::ArchivePage {
            metadata: session.metadata().clone(),
            entries: session.entries()[offset..end].to_vec(),
            next: (end < session.entries().len()).then(|| super::ArchiveCursor {
                session_id: id.into(),
                revision: session.metadata().revision,
                offset: end,
            }),
        })
    }
    fn navigate_window(
        &self,
        _window: &mut super::SessionWindow,
        _action: super::ArchiveNavigation,
    ) -> Result<()> {
        anyhow::bail!("repository does not support window navigation")
    }
    fn session_info(&self, id: &str) -> Result<SessionInfo> {
        self.open(id).map(|session| SessionInfo::from(&session))
    }
    fn tree_page(
        &self,
        id: &str,
        limit: usize,
        cursor: Option<&super::TreeCursor>,
    ) -> Result<super::TreePage> {
        self.open(id)?.tree_page(limit, cursor)
    }
    fn search_page(
        &self,
        id: &str,
        query: &str,
        limit: usize,
        cursor: Option<&super::SearchCursor>,
    ) -> Result<super::SearchPage> {
        self.open(id)?.search_messages(query, limit, cursor)
    }
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
