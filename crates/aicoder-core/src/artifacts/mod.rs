//! Opaque bounded output artifacts. Hosts own scope, storage and retention.
mod memory;
mod read_tool;
use async_trait::async_trait;
pub use memory::MemoryArtifactStore;
pub use read_tool::ReadArtifactTool;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArtifactLimits {
    pub max_artifact_bytes: u64,
    pub max_total_bytes: u64,
    pub max_artifacts: usize,
}
impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            max_artifact_bytes: 16 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_artifacts: 128,
        }
    }
}
impl ArtifactLimits {
    pub fn validate(self) -> Result<Self, ArtifactError> {
        if !(1..=1024 * 1024 * 1024).contains(&self.max_artifact_bytes)
            || self.max_total_bytes < self.max_artifact_bytes
            || self.max_total_bytes > 1024 * 1024 * 1024 * 1024
            || !(1..=65536).contains(&self.max_artifacts)
        {
            return Err(ArtifactError::new("invalid_limits"));
        }
        Ok(self)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub version: u32,
    pub id: String,
    pub media_type: String,
    pub bytes: u64,
    /// All bytes of this source were captured; false identifies a partial capture.
    pub complete: bool,
}
impl ArtifactRef {
    pub fn validate(&self) -> Result<(), ArtifactError> {
        if self.bytes > 1024 * 1024 * 1024
            || self.version != 1
            || !valid_id(&self.id)
            || self.media_type.is_empty()
            || self.media_type.len() > 128
            || self.media_type.chars().any(char::is_control)
        {
            return Err(ArtifactError::new("invalid_reference"));
        }
        Ok(())
    }
}
pub fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.hyphenated().to_string() == id)
}
#[derive(Debug, Clone)]
pub struct ArtifactPage {
    pub artifact: ArtifactRef,
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub next_offset: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactError {
    pub code: &'static str,
}
impl ArtifactError {
    pub const fn new(code: &'static str) -> Self {
        Self { code }
    }
}
impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Artifact {}", self.code)
    }
}
impl std::error::Error for ArtifactError {}
pub const MAX_ARTIFACT_PAGE_BYTES: usize = 64 * 1024;
/// A scoped store is explicit host authority. References contain no filesystem paths.
#[async_trait]
pub trait ArtifactStore: Send + Sync {
    fn limits(&self) -> ArtifactLimits;
    async fn begin(&self, media_type: &str) -> Result<Box<dyn ArtifactWriter>, ArtifactError>;
    async fn read(
        &self,
        id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<ArtifactPage, ArtifactError>;
    /// Explicit host retention operation, never exposed as an implicit model action.
    async fn remove(&self, id: &str) -> Result<(), ArtifactError>;
}
/// Dropping an unfinished writer releases its reservation and discards staged bytes.
#[async_trait]
pub trait ArtifactWriter: Send + Sync {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), ArtifactError>;
    async fn finish(self: Box<Self>, complete: bool) -> Result<ArtifactRef, ArtifactError>;
}
pub async fn store_bytes(
    store: &dyn ArtifactStore,
    media_type: &str,
    bytes: &[u8],
    complete: bool,
) -> Result<ArtifactRef, ArtifactError> {
    if bytes.len() as u64 > store.limits().max_artifact_bytes {
        return Err(ArtifactError::new("too_large"));
    }
    let mut writer = store.begin(media_type).await?;
    for chunk in bytes.chunks(16384) {
        writer.write(chunk).await?;
    }
    writer.finish(complete).await
}
pub(crate) fn page_bounds(bytes: u64, offset: u64, limit: usize) -> Result<usize, ArtifactError> {
    if offset > bytes || limit == 0 || limit > MAX_ARTIFACT_PAGE_BYTES {
        return Err(ArtifactError::new("invalid_page"));
    }
    Ok((bytes - offset).min(limit as u64) as usize)
}
