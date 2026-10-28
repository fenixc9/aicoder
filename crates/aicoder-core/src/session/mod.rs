//! Conversation session management and persistence.

mod jsonl;
mod legacy;
const SESSION_VERSION: u32 = 2;
mod memory;
mod repository;
mod types;

pub use jsonl::JsonlSessionRepository;
pub use memory::MemorySessionRepository;
pub use repository::SessionRepository;
pub use types::{Session, SessionInfo, SessionMessage, SessionMetadata};
