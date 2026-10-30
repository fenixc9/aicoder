//! Conversation session management and persistence.

mod journal;
pub(crate) use journal::SessionJournal;
pub const SESSION_VERSION: u32 = 3;
mod memory;
mod model_selection;
pub(crate) mod navigation;
pub use model_selection::*;
mod projection;
mod reasoning;
mod repository;
pub use reasoning::*;
mod types;
mod window;
pub use navigation::*;
pub use projection::{
    BranchAccounting, ContextOrigin, SessionBranch, SessionContext, SessionContextSource,
};
pub use window::*;

pub use memory::MemorySessionRepository;
pub use repository::{SessionConflict, SessionRepository, SessionRunLease};
pub use types::{
    Session, SessionEntry, SessionEntryData, SessionInfo, SessionMessage, SessionMetadata,
    SessionTransaction, TurnRecord, TurnStatistics,
};

fn now_millis() -> anyhow::Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}
fn sort_sessions(sessions: &mut [SessionInfo]) {
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });
}
