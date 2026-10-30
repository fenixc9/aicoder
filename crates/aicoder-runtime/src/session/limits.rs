use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionLoadLimits {
    pub max_snapshot_bytes: u64,
    pub max_session_files: usize,
    pub max_record_bytes: usize,
    pub max_window_bytes: usize,
    pub max_window_messages: usize,
    pub max_index_entries: usize,
    pub max_index_bytes: usize,
    pub max_scan_bytes: u64,
    pub max_page_bytes: usize,
}
impl Default for SessionLoadLimits {
    fn default() -> Self {
        Self {
            max_snapshot_bytes: 16 * 1024 * 1024,
            max_session_files: 10000,
            max_record_bytes: 8 * 1024 * 1024,
            max_window_bytes: 2 * 1024 * 1024,
            max_window_messages: 2048,
            max_index_entries: 100000,
            max_index_bytes: 32 * 1024 * 1024,
            max_scan_bytes: 4 * 1024 * 1024 * 1024,
            max_page_bytes: 8 * 1024 * 1024 + 64 * 1024,
        }
    }
}
impl SessionLoadLimits {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=100000).contains(&self.max_session_files)
                && (1..=256 * 1024 * 1024).contains(&self.max_snapshot_bytes)
                && (1..=64 * 1024 * 1024).contains(&self.max_record_bytes)
                && (1..=64 * 1024 * 1024).contains(&self.max_window_bytes)
                && (1..=100000).contains(&self.max_window_messages)
                && (1..=1000000).contains(&self.max_index_entries)
                && (1..=512 * 1024 * 1024).contains(&self.max_index_bytes)
                && self.max_scan_bytes >= self.max_snapshot_bytes
                && self.max_scan_bytes <= 1024 * 1024 * 1024 * 1024
                && (1..=128 * 1024 * 1024).contains(&self.max_page_bytes),
            "invalid session load limits"
        );
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct SessionLoadLimit {
    pub resource: &'static str,
    pub limit: u64,
    pub requested: u64,
}
impl std::fmt::Display for SessionLoadLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Session {} load budget exceeded: {} > {}; use archive pages or adjust trusted host limits",
            self.resource, self.requested, self.limit
        )
    }
}
impl std::error::Error for SessionLoadLimit {}
pub(super) fn check(resource: &'static str, requested: u64, limit: u64) -> anyhow::Result<()> {
    if requested > limit {
        return Err(SessionLoadLimit {
            resource,
            limit,
            requested,
        }
        .into());
    }
    Ok(())
}
