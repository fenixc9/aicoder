//! Bounded canonical session interchange and explicit host-owned publication.
mod render;
mod storage;
use crate::session::JsonlSessionRepository;
use aicoder_core::{
    TurnExecutionContext,
    session::{SessionEntry, SessionMetadata},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
pub use storage::{CommitFailure, DirectoryPublisher};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Json,
    Html,
}
impl std::str::FromStr for ExportFormat {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "json" => Ok(Self::Json),
            "html" => Ok(Self::Html),
            _ => Err("export format must be json or html".into()),
        }
    }
}
impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Html => "html",
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ExportLimits {
    pub max_source_bytes: u64,
    pub max_entries: usize,
    pub max_output_bytes: usize,
}
impl Default for ExportLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 128 * 1024 * 1024,
            max_entries: 10000,
            max_output_bytes: 16 * 1024 * 1024,
        }
    }
}
impl ExportLimits {
    pub fn validate(self) -> Result<Self> {
        ensure!(
            (1..=1024 * 1024 * 1024).contains(&self.max_source_bytes)
                && (1..=100000).contains(&self.max_entries)
                && (1..=64 * 1024 * 1024).contains(&self.max_output_bytes),
            "invalid export limits"
        );
        Ok(self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub metadata: SessionMetadata,
    pub scope: String,
    pub entry_count: usize,
    pub source_bytes: u64,
    pub ignored_tail_bytes: u64,
    pub source_tail_complete: bool,
    pub unfinished_turn: Option<String>,
    pub includes_artifact_bytes: bool,
}
impl Manifest {
    pub(crate) fn new(
        metadata: SessionMetadata,
        entry_count: usize,
        source_bytes: u64,
        ignored_tail_bytes: u64,
        unfinished_turn: Option<String>,
    ) -> Self {
        Self {
            metadata,
            scope: "all_canonical_branches".into(),
            entry_count,
            source_bytes,
            ignored_tail_bytes,
            source_tail_complete: ignored_tail_bytes == 0,
            unfinished_turn,
            includes_artifact_bytes: false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportReceipt {
    pub version: u32,
    pub session_id: String,
    pub revision: u64,
    pub selected_leaf: Option<String>,
    pub format: ExportFormat,
    pub entries: usize,
    pub bytes: usize,
    pub canonical_entries_complete: bool,
    pub source_tail_complete: bool,
    /// Non-cryptographic content identifier; never an authorization token or signature.
    pub fingerprint: String,
}
/// Immutable owned bytes validated against the source before becoming available to publishers.
#[derive(Clone)]
pub struct PreparedExport {
    bytes: Arc<[u8]>,
    receipt: ExportReceipt,
}
impl PreparedExport {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn receipt(&self) -> &ExportReceipt {
        &self.receipt
    }
    /// Create a new private file, without overwriting existing output or session data.
    pub fn write_new(
        &self,
        path: &Path,
        context: &TurnExecutionContext,
    ) -> Result<PublishedExport> {
        storage::write_new(self, path, None, context)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedExport {
    pub export: ExportReceipt,
    pub path: PathBuf,
    pub url: Option<String>,
}
/// Explicit host capability. Implementations own destination authentication and cancellation.
/// Export preparation never invokes this interface or retries a failed publication.
#[async_trait::async_trait]
pub trait ExportPublisher: Send + Sync {
    async fn publish(
        &self,
        export: &PreparedExport,
        context: TurnExecutionContext,
    ) -> Result<PublishedExport>;
}
pub fn prepare(
    repository: &JsonlSessionRepository,
    workspace: &Path,
    id: &str,
    format: ExportFormat,
    limits: ExportLimits,
    context: &TurnExecutionContext,
) -> Result<PreparedExport> {
    let limits = limits.validate()?;
    crate::session::visit_export(
        repository,
        workspace,
        id,
        &limits,
        context,
        |manifest, entries, selected| render(manifest, entries, selected, format, limits, context),
    )
}
fn render(
    manifest: Manifest,
    entries: &mut dyn Iterator<Item = Result<SessionEntry>>,
    selected: &HashSet<String>,
    format: ExportFormat,
    limits: ExportLimits,
    context: &TurnExecutionContext,
) -> Result<PreparedExport> {
    let mut output = Bounded {
        bytes: Vec::new(),
        limit: limits.max_output_bytes,
        context,
    };
    render::document(&mut output, &manifest, entries, selected, format)?;
    let fingerprint = format!(
        "fnv1a64:{:016x}",
        output
            .bytes
            .iter()
            .fold(0xcbf29ce484222325u64, |hash, b| (hash ^ u64::from(*b))
                .wrapping_mul(0x100000001b3))
    );
    let receipt = ExportReceipt {
        version: 1,
        session_id: manifest.metadata.id,
        revision: manifest.metadata.revision,
        selected_leaf: manifest.metadata.leaf_id,
        format,
        entries: manifest.entry_count,
        bytes: output.bytes.len(),
        canonical_entries_complete: true,
        source_tail_complete: manifest.source_tail_complete,
        fingerprint,
    };
    Ok(PreparedExport {
        bytes: output.bytes.into(),
        receipt,
    })
}
struct Bounded<'a> {
    bytes: Vec<u8>,
    limit: usize,
    context: &'a TurnExecutionContext,
}
impl Write for Bounded<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.context.is_cancelled() {
            return Err(io::Error::other("export cancelled"));
        }
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("export encoded output budget exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
