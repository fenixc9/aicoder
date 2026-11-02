use anyhow::{Result, ensure};
use serde::Serialize;
use std::io::Write;
pub(crate) struct LimitedWriter<W> {
    pub inner: W,
    pub remaining: usize,
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::Error::other(
                "evaluation serialization budget exhausted",
            ));
        }
        self.inner.write_all(bytes)?;
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
pub(crate) fn encoded_size(value: &impl Serialize, limit: usize) -> Result<usize> {
    let mut writer = LimitedWriter {
        inner: std::io::sink(),
        remaining: limit,
    };
    serde_json::to_writer(&mut writer, value)?;
    Ok(limit - writer.remaining)
}
pub(crate) fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    let mut writer = LimitedWriter {
        inner: Vec::new(),
        remaining: limit,
    };
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.inner)
}
pub(crate) fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(crate) fn read(path: &std::path::Path, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let before = std::fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && !before.file_type().is_symlink(),
        "evaluation input must be a regular file"
    );
    ensure!(
        before.len() <= limit as u64,
        "evaluation input exceeds its byte budget"
    );
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file() && stamp(&before) == stamp(&file.metadata()?),
        "evaluation input changed before capture"
    );
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit
            && stamp(&before) == stamp(&file.metadata()?)
            && stamp(&before) == stamp(&std::fs::symlink_metadata(path)?),
        "evaluation input changed or grew during capture"
    );
    Ok(bytes)
}
#[cfg(unix)]
fn stamp(meta: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}
#[cfg(not(unix))]
fn stamp(meta: &std::fs::Metadata) -> (u64, Option<std::time::SystemTime>) {
    (meta.len(), meta.modified().ok())
}
