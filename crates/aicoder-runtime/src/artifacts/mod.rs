//! Private filesystem artifacts, scoped to a canonical workspace and bounded by host policy.
use aicoder_core::artifacts::*;
use async_trait::async_trait;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncWriteExt;
const HEADER_MAX: u64 = 16 * 1024;
#[derive(Clone)]
pub struct FileArtifactStore {
    root: PathBuf,
    workspace: PathBuf,
    limits: ArtifactLimits,
    lock_timeout: Duration,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Footer {
    workspace: PathBuf,
    reference: ArtifactRef,
}
fn storage(_: impl std::fmt::Display) -> ArtifactError {
    ArtifactError::new("storage_failed")
}
fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    options
}
impl FileArtifactStore {
    /// Does not create storage until the first write. Root is trusted host configuration.
    pub fn new(
        root: impl AsRef<Path>,
        workspace: impl AsRef<Path>,
        limits: ArtifactLimits,
    ) -> anyhow::Result<Self> {
        let workspace = workspace.as_ref().canonicalize()?;
        anyhow::ensure!(workspace.is_dir(), "Artifact workspace must be a directory");
        Ok(Self {
            root: if root.as_ref().is_absolute() {
                root.as_ref().to_path_buf()
            } else {
                std::env::current_dir()?.join(root.as_ref())
            },
            workspace,
            limits: limits.validate()?,
            lock_timeout: Duration::from_millis(100),
        })
    }
    /// Bound metadata-lock contention using trusted host policy, without changing IO timeouts.
    pub fn with_lock_timeout(mut self, timeout: Duration) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (Duration::from_millis(1)..=Duration::from_secs(5)).contains(&timeout),
            "Artifact lock timeout must be 1 ms to 5 s"
        );
        self.lock_timeout = timeout;
        Ok(self)
    }
    fn initialize(&self) -> Result<(), ArtifactError> {
        fs::create_dir_all(&self.root).map_err(storage)?;
        let meta = fs::symlink_metadata(&self.root).map_err(storage)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(ArtifactError::new("invalid_storage"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700)).map_err(storage)?;
        }
        Ok(())
    }
    fn lock(&self) -> Result<File, ArtifactError> {
        self.initialize()?;
        let file = options()
            .read(true)
            .write(true)
            .create(true)
            .open(self.root.join(".lock"))
            .map_err(storage)?;
        if !file.metadata().map_err(storage)?.is_file() {
            return Err(ArtifactError::new("invalid_storage"));
        }
        let deadline = std::time::Instant::now() + self.lock_timeout;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(file),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(ArtifactError::new("busy"));
                }
                Err(error) => return Err(storage(error)),
            }
        }
    }
    fn files(&self) -> Result<Vec<fs::DirEntry>, ArtifactError> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.root)
            .map_err(storage)?
            .take(self.limits.max_artifacts.saturating_mul(2) + 65)
        {
            files.push(entry.map_err(storage)?);
        }
        if files.len() > self.limits.max_artifacts.saturating_mul(2) + 64 {
            return Err(ArtifactError::new("directory_limit"));
        }
        Ok(files)
    }
    fn reserve(&self, media_type: String) -> Result<Writer, ArtifactError> {
        let _lock = self.lock()?;
        let mut count = 0;
        let mut used = 0u64;
        for entry in self.files()? {
            if matches!(
                entry.path().extension().and_then(|s| s.to_str()),
                Some("part" | "artifact")
            ) {
                let meta = match fs::symlink_metadata(entry.path()) {
                    Ok(meta) => meta,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(storage(error)),
                };
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err(ArtifactError::new("invalid_storage"));
                }
                count += 1;
                used = used.saturating_add(meta.len());
            }
        }
        if count >= self.limits.max_artifacts
            || used.saturating_add(self.limits.max_artifact_bytes) > self.limits.max_total_bytes
        {
            return Err(ArtifactError::new("quota_exceeded"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let reference = ArtifactRef {
            version: 1,
            id: id.clone(),
            media_type,
            bytes: 0,
            complete: false,
        };
        reference.validate()?;
        let path = self.root.join(format!("{id}.part"));
        let file = options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(storage)?;
        if let Err(error) = file
            .try_lock_exclusive()
            .and_then(|()| file.set_len(self.limits.max_artifact_bytes))
        {
            let _ = fs::remove_file(&path);
            return Err(storage(error));
        }
        let writer = Writer {
            store: self.clone(),
            reference,
            file: tokio::fs::File::from_std(file),
            path,
            written: 0,
        };
        // Sparse logical reservation bounds concurrent and cross-process writers without
        // holding the store lock for the command's lifetime.
        Ok(writer)
    }
    fn open(&self, id: &str) -> Result<(File, ArtifactRef), ArtifactError> {
        if !valid_id(id) {
            return Err(ArtifactError::new("invalid_reference"));
        }
        let path = self.root.join(format!("{id}.artifact"));
        let mut file = options().read(true).open(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ArtifactError::new("not_found")
            } else {
                storage(error)
            }
        })?;
        let meta = file.metadata().map_err(storage)?;
        let len = meta.len();
        if !meta.is_file() || !(8..=1024 * 1024 * 1024 + HEADER_MAX + 8).contains(&len) {
            return Err(ArtifactError::new("corrupt"));
        }
        file.seek(SeekFrom::End(-8)).map_err(storage)?;
        let mut size = [0; 8];
        file.read_exact(&mut size).map_err(storage)?;
        let size = u64::from_le_bytes(size);
        if size > HEADER_MAX || size + 8 > len {
            return Err(ArtifactError::new("corrupt"));
        }
        file.seek(SeekFrom::Start(len - size - 8))
            .map_err(storage)?;
        let mut data = vec![0; size as usize];
        file.read_exact(&mut data).map_err(storage)?;
        let footer: Footer =
            serde_json::from_slice(&data).map_err(|_| ArtifactError::new("corrupt"))?;
        footer.reference.validate()?;
        if footer.workspace != self.workspace {
            return Err(ArtifactError::new("scope_mismatch"));
        }
        if footer.reference.id != id || footer.reference.bytes != len - size - 8 {
            return Err(ArtifactError::new("corrupt"));
        }
        Ok((file, footer.reference))
    }
    /// Recover reservations left by vanished processes. Live writers retain an OS lock.
    pub async fn reap_incomplete(&self, minimum_age: Duration) -> Result<usize, ArtifactError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = store.lock()?;
            let mut removed = 0;
            for entry in store.files()? {
                if entry.path().extension().is_none_or(|s| s != "part") {
                    continue;
                }
                let file = match options().read(true).write(true).open(entry.path()) {
                    Ok(file) => file,
                    Err(_) => continue,
                };
                if !file.metadata().map_err(storage)?.is_file()
                    || file.try_lock_exclusive().is_err()
                {
                    continue;
                }
                let age = file
                    .metadata()
                    .map_err(storage)?
                    .modified()
                    .map_err(storage)?
                    .elapsed()
                    .unwrap_or_default();
                if age >= minimum_age {
                    fs::remove_file(entry.path()).map_err(storage)?;
                    removed += 1;
                }
            }
            Ok(removed)
        })
        .await
        .map_err(storage)?
    }
}
struct Writer {
    store: FileArtifactStore,
    reference: ArtifactRef,
    file: tokio::fs::File,
    path: PathBuf,
    written: u64,
}
impl Drop for Writer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
#[async_trait]
impl ArtifactWriter for Writer {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), ArtifactError> {
        if self.written.saturating_add(bytes.len() as u64) > self.store.limits.max_artifact_bytes {
            return Err(ArtifactError::new("too_large"));
        }
        self.file.write_all(bytes).await.map_err(storage)?;
        self.written += bytes.len() as u64;
        Ok(())
    }
    async fn finish(mut self: Box<Self>, complete: bool) -> Result<ArtifactRef, ArtifactError> {
        self.reference.bytes = self.written;
        self.reference.complete = complete;
        let footer = serde_json::to_vec(&Footer {
            workspace: self.store.workspace.clone(),
            reference: self.reference.clone(),
        })
        .map_err(storage)?;
        if footer.len() as u64 > HEADER_MAX {
            return Err(ArtifactError::new("metadata_too_large"));
        }
        self.file.write_all(&footer).await.map_err(storage)?;
        self.file
            .write_all(&(footer.len() as u64).to_le_bytes())
            .await
            .map_err(storage)?;
        self.file
            .set_len(self.written + footer.len() as u64 + 8)
            .await
            .map_err(storage)?;
        self.file.flush().await.map_err(storage)?;
        self.file.sync_all().await.map_err(storage)?;
        // Publication is one rename after the complete footer is durable. A cancelled
        // publication may leave an unreferenced final file, charged to the same quota.
        let from = self.path.clone();
        let to = self
            .store
            .root
            .join(format!("{}.artifact", self.reference.id));
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = store.lock()?;
            fs::rename(from, to).map_err(storage)?;
            File::open(&store.root)
                .and_then(|file| file.sync_all())
                .map_err(storage)
        })
        .await
        .map_err(storage)??;
        Ok(self.reference.clone())
    }
}
#[async_trait]
impl ArtifactStore for FileArtifactStore {
    fn limits(&self) -> ArtifactLimits {
        self.limits
    }
    async fn begin(&self, media_type: &str) -> Result<Box<dyn ArtifactWriter>, ArtifactError> {
        let store = self.clone();
        let media_type = media_type.to_string();
        let writer = tokio::task::spawn_blocking(move || {
            let writer = store.reserve(media_type)?;
            // Reserve and lock before releasing admission ownership.
            Ok::<_, ArtifactError>(writer)
        })
        .await
        .map_err(storage)??;
        Ok(Box::new(writer))
    }
    async fn read(
        &self,
        id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<ArtifactPage, ArtifactError> {
        let store = self.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            let (mut file, artifact) = store.open(&id)?;
            if offset > artifact.bytes || limit == 0 || limit > MAX_ARTIFACT_PAGE_BYTES {
                return Err(ArtifactError::new("invalid_page"));
            }
            let count = (artifact.bytes - offset).min(limit as u64) as usize;
            file.seek(SeekFrom::Start(offset)).map_err(storage)?;
            let mut bytes = vec![0; count];
            file.read_exact(&mut bytes).map_err(storage)?;
            Ok(ArtifactPage {
                next_offset: (offset + (count as u64) < artifact.bytes)
                    .then_some(offset + count as u64),
                artifact,
                offset,
                bytes,
            })
        })
        .await
        .map_err(storage)?
    }
    async fn remove(&self, id: &str) -> Result<(), ArtifactError> {
        let store = self.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            let _lock = store.lock()?;
            store.open(&id)?;
            fs::remove_file(store.root.join(format!("{id}.artifact"))).map_err(storage)
        })
        .await
        .map_err(storage)?
    }
}
