//! Conversation session management and persistence.
mod native_context;
pub use native_context::{NATIVE_CONTEXT_NAMESPACE, NativeContextSource};

mod journal;
pub(crate) use journal::SessionJournal;
pub const SESSION_VERSION: u32 = 3;
mod append;
mod context_edit;
pub use append::SessionAppendPlan;
mod fork;
pub use context_edit::{
    CONTEXT_EDIT_NAMESPACE, ContextEditRecord, ContextReplacement, MAX_CONTEXT_EDIT_BYTES,
};
mod lifecycle;
mod memory;
pub use fork::*;
mod model_selection;
pub use lifecycle::{
    SessionActionReport, SessionCreatedWithoutOwnership, SessionNavigationOutcome,
    SessionSelectionOutcome,
};
pub(crate) mod navigation;
pub use model_selection::*;
mod plan;
mod projection;
mod reasoning;
mod repository;
pub use plan::{PreparedSessionNavigation, SessionNavigationPlan, prepare_transaction};
pub use reasoning::*;
mod types;
mod window;
pub use navigation::*;
pub use projection::{
    BranchAccounting, ContextOrigin, OwnedSessionContext, SessionBranch, SessionContext,
    SessionContextSource,
};
pub use window::*;

pub use memory::MemorySessionRepository;
pub use repository::{SessionConflict, SessionExtensionState, SessionRepository, SessionRunLease};
pub use types::{
    Session, SessionEntry, SessionEntryData, SessionInfo, SessionMessage, SessionMetadata,
    SessionTransaction, TurnRecord, TurnStatistics,
};

pub(crate) fn now_millis() -> anyhow::Result<u64> {
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
