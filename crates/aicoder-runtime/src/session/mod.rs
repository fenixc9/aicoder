//! Filesystem session repositories; domain types and memory storage live in core.
mod archive;
pub(crate) use archive::visit_export;
mod jsonl;
mod limits;
pub use limits::{SessionLoadLimit, SessionLoadLimits};
mod legacy;
use aicoder_core::session::*;
pub use jsonl::JsonlSessionRepository;
use std::path::PathBuf;
#[derive(Debug, Clone)]
struct StorageCursor {
    path: PathBuf,
    expected_len: u64,
    valid_len: u64,
    fingerprint: u64,
    needs_separator: bool,
}
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
fn attach_cursor(session: &mut Session, cursor: StorageCursor) {
    let path = cursor.path.clone();
    session.attach_backend_state(cursor, Some(path));
}
