//! Shared bounds and opaque, query-scoped continuation state for read-only tools.
use super::{ToolContext, ToolFailure, ToolSuccess};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{future::Future, path::Path, time::UNIX_EPOCH};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncBufRead, AsyncBufReadExt},
};

pub(super) const MAX_PAGE_BYTES: usize = 64 * 1024;
pub(super) const MAX_SCAN_BYTES: usize = 16 * 1024 * 1024;
pub(super) const MAX_LINE_BYTES: usize = 64 * 1024;
pub(super) const MAX_ENTRIES: usize = 20_000;
pub(super) const MAX_PATH_BYTES: usize = 4096;

pub(super) fn failure(code: &str, message: impl Into<String>) -> ToolFailure {
    ToolFailure::new(code, message)
}
pub(super) fn io_error(error: std::io::Error) -> ToolFailure {
    failure("read_failed", error.to_string())
}
pub(super) fn page_limit(context: &ToolContext) -> Result<usize, ToolFailure> {
    let limit = context.max_output_bytes().min(MAX_PAGE_BYTES);
    if limit < 512 {
        return Err(too_small());
    }
    Ok(limit)
}
pub(super) fn too_small() -> ToolFailure {
    failure(
        "output_limit_too_small",
        "Output budget cannot hold a complete record and continuation cursor; increase max_output_bytes",
    )
}
pub(super) fn success(output: Value, limit: usize) -> Result<ToolSuccess, ToolFailure> {
    if output.to_string().len() > limit {
        return Err(too_small());
    }
    Ok(ToolSuccess {
        artifacts: Vec::new(),
        truncated: output["has_more"] == true || output["content_truncated"] == true,
        output,
    })
}
pub(super) async fn bounded<T>(
    context: &ToolContext,
    work: impl Future<Output = Result<T, ToolFailure>>,
) -> Result<T, ToolFailure> {
    tokio::time::timeout(context.tool_timeout(), work)
        .await
        .map_err(|_| failure("timeout", "Read-only tool exceeded its time limit"))?
}
pub(super) fn key(value: &impl Serialize) -> String {
    // Stable identity for accidental query mix-ups, not an authentication signature.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in serde_json::to_vec(value).expect("query is serializable") {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileStamp {
    pub len: u64,
    seconds: u64,
    nanos: u32,
    #[cfg(unix)]
    file_id: (u64, u64),
}
impl FileStamp {
    pub fn from(metadata: &std::fs::Metadata) -> Result<Self, ToolFailure> {
        let time = metadata
            .modified()
            .map_err(io_error)?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| failure("read_failed", "File timestamp precedes the Unix epoch"))?;
        Ok(Self {
            len: metadata.len(),
            seconds: time.as_secs(),
            nanos: time.subsec_nanos(),
            #[cfg(unix)]
            file_id: {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            },
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Cursor {
    version: u8,
    query: String,
    pub path: String,
    pub offset: u64,
    pub line: u64,
    pub stamp: Option<FileStamp>,
}
impl Cursor {
    pub fn new(
        query: &str,
        path: String,
        offset: u64,
        line: u64,
        stamp: Option<FileStamp>,
    ) -> Self {
        Self {
            version: 1,
            query: query.into(),
            path,
            offset,
            line,
            stamp,
        }
    }
    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("cursor is serializable")
    }
    pub fn decode(value: &str, query: &str) -> Result<Self, ToolFailure> {
        if value.len() > 16 * 1024 {
            return Err(failure("invalid_cursor", "Cursor is too large"));
        }
        let cursor: Self = serde_json::from_str(value)
            .map_err(|_| failure("invalid_cursor", "Invalid continuation cursor"))?;
        if cursor.version != 1
            || cursor.query != query
            || cursor.path.len() > MAX_PATH_BYTES
            || cursor.line == 0
            || cursor.line > u64::MAX - MAX_SCAN_BYTES as u64
        {
            return Err(failure(
                "invalid_cursor",
                "Cursor belongs to a different query or version",
            ));
        }
        Ok(cursor)
    }
    pub fn check_file(&self, stamp: &FileStamp) -> Result<(), ToolFailure> {
        if self.stamp.as_ref() != Some(stamp) || self.offset > stamp.len {
            return Err(failure(
                "stale_cursor",
                "File changed since the previous page; restart this file/query",
            ));
        }
        Ok(())
    }
}
pub(super) async fn open_regular(path: &Path) -> Result<(File, FileStamp), ToolFailure> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_NOCTTY);
    let file = options.open(path).await.map_err(io_error)?;
    let metadata = file.metadata().await.map_err(io_error)?;
    if !metadata.is_file() {
        return Err(failure("not_a_file", "Path is not a regular file"));
    }
    let stamp = FileStamp::from(&metadata)?;
    Ok((file, stamp))
}
pub(super) async fn unchanged(file: &File, stamp: &FileStamp) -> Result<(), ToolFailure> {
    if FileStamp::from(&file.metadata().await.map_err(io_error)?)? != *stamp {
        return Err(failure(
            "stale_cursor",
            "File changed while reading; restart the query",
        ));
    }
    Ok(())
}
pub(super) fn utf8_prefix(bytes: &[u8]) -> Result<&str, ToolFailure> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(error) if error.error_len().is_none() => {
            Ok(std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("validated prefix"))
        }
        Err(_) => Err(failure("non_utf8", "Text tool requires valid UTF-8")),
    }
}
pub(super) enum LineRead {
    Line(Vec<u8>),
    Eof,
    ScanLimit,
    Binary,
}
/// Never allocates an unbounded line, even when the file has no newline.
pub(super) async fn next_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    scanned: &mut usize,
) -> Result<LineRead, ToolFailure> {
    let mut line = Vec::new();
    loop {
        if *scanned >= MAX_SCAN_BYTES {
            return Ok(LineRead::ScanLimit);
        }
        let bytes = reader.fill_buf().await.map_err(io_error)?;
        if bytes.is_empty() {
            return Ok(if line.is_empty() {
                LineRead::Eof
            } else {
                LineRead::Line(line)
            });
        }
        let count = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1)
            .min(MAX_SCAN_BYTES - *scanned);
        if bytes[..count].contains(&0) {
            // Account for the buffered bytes discarded when skipping this file.
            *scanned += bytes.len().min(MAX_SCAN_BYTES - *scanned);
            return Ok(LineRead::Binary);
        }
        if line.len() + count > MAX_LINE_BYTES {
            return Err(failure(
                "line_too_long",
                "Search line exceeds 64 KiB; inspect it with read_file's byte-continuation pages",
            ));
        }
        line.extend_from_slice(&bytes[..count]);
        *scanned += count;
        reader.consume(count);
        if line.ends_with(b"\n") {
            return Ok(LineRead::Line(line));
        }
    }
}
