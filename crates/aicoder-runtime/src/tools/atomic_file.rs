//! Prepared file replacements: pinned parents, bounded snapshots and guarded publication.
use super::{
    PreparedTool, ToolContext, ToolFailure, ToolPreview, ToolSuccess, WorkspacePaths,
    io::{FileStamp, key},
    util::truncate_utf8,
};
use async_trait::async_trait;
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions, Permissions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncWriteExt;

pub(super) const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
fn fail(code: &str, message: impl Into<String>) -> ToolFailure {
    ToolFailure::new(code, message)
}
fn io(error: std::io::Error) -> ToolFailure {
    fail("write_failed", error.to_string())
}
fn conflict() -> ToolFailure {
    fail(
        "edit_conflict",
        "File or its path changed since preparation; read it again and prepare a new operation",
    )
}
pub(super) fn revision(stamp: &FileStamp) -> String {
    format!("v1:{}", key(stamp))
}

#[derive(Clone)]
struct Parent {
    file: Arc<File>,
    path: PathBuf,
}
impl Parent {
    fn open(path: &Path) -> Result<Self, ToolFailure> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(io)?;
        let parent = Self {
            file: Arc::new(file),
            path: path.into(),
        };
        parent.check()?;
        Ok(parent)
    }
    fn check(&self) -> Result<(), ToolFailure> {
        let current = fs::symlink_metadata(&self.path).map_err(|_| conflict())?;
        let pinned = self.file.metadata().map_err(io)?;
        if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
            return Err(conflict());
        }
        Ok(())
    }
    fn open_at(&self, name: &CString, flags: i32, mode: u32) -> std::io::Result<File> {
        // SAFETY: both the held descriptor and NUL-terminated name remain valid.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                mode as libc::mode_t,
            )
        };
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    fn unlink(&self, name: &CString) {
        unsafe {
            libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0);
        }
    }
}
struct Temporary {
    parent: Parent,
    name: CString,
}
impl Drop for Temporary {
    fn drop(&mut self) {
        self.parent.unlink(&self.name);
    }
}
struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

#[derive(Clone)]
pub(super) struct Snapshot {
    pub bytes: Arc<[u8]>,
    pub stamp: FileStamp,
    permissions: Permissions,
}
#[derive(Clone)]
pub(super) struct Target {
    parent: Parent,
    name: CString,
    pub path: PathBuf,
    pub argument: String,
    pub original: Option<Snapshot>,
}
impl Target {
    pub fn load(context: &ToolContext, argument: &str) -> Result<Self, ToolFailure> {
        if argument.len() > 4096 {
            return Err(fail("invalid_arguments", "path exceeds 4096 bytes"));
        }
        let path = WorkspacePaths::from_context(context).resolve_for_write(argument)?;
        if path.to_str().is_none() {
            return Err(fail(
                "unsupported_path",
                "File mutation requires UTF-8 paths",
            ));
        }
        let parent = Parent::open(
            path.parent()
                .ok_or_else(|| fail("invalid_path", "File has no parent"))?,
        )?;
        let name = CString::new(
            path.file_name()
                .ok_or_else(|| fail("invalid_path", "File has no name"))?
                .as_encoded_bytes(),
        )
        .map_err(|_| fail("invalid_path", "Invalid filename"))?;
        let original = Self::read(&parent, &name)?;
        Ok(Self {
            parent,
            name,
            path,
            argument: argument.into(),
            original,
        })
    }
    fn read(parent: &Parent, name: &CString) -> Result<Option<Snapshot>, ToolFailure> {
        let mut file =
            match parent.open_at(name, libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY, 0) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(io(error)),
            };
        let metadata = file.metadata().map_err(io)?;
        if !metadata.is_file() {
            return Err(fail("not_a_file", "Mutation requires a regular file"));
        }
        if metadata.len() > MAX_FILE_BYTES as u64 {
            return Err(fail(
                "file_too_large",
                "File exceeds the 4 MiB mutation limit",
            ));
        }
        let stamp = FileStamp::from(&metadata)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(fail(
                "file_too_large",
                "File exceeds the 4 MiB mutation limit",
            ));
        }
        let after = file.metadata().map_err(io)?;
        if FileStamp::from(&after)? != stamp || metadata.mode() != after.mode() {
            return Err(conflict());
        }
        Ok(Some(Snapshot {
            bytes: bytes.into(),
            stamp,
            permissions: metadata.permissions(),
        }))
    }
    pub fn expect(&self, expected: Option<&str>) -> Result<(), ToolFailure> {
        let actual = self
            .original
            .as_ref()
            .map_or_else(|| "missing".into(), |s| revision(&s.stamp));
        if expected.is_some_and(|expected| expected != actual) {
            return Err(conflict());
        }
        Ok(())
    }
    fn unchanged(&self) -> Result<(), ToolFailure> {
        self.parent.check()?;
        let current = Self::read(&self.parent, &self.name)?;
        let equal = match (&self.original, &current) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                a.stamp == b.stamp
                    && a.bytes == b.bytes
                    && a.permissions.mode() == b.permissions.mode()
            }
            _ => false,
        };
        if !equal {
            return Err(conflict());
        }
        Ok(())
    }
    async fn validate_snapshot(&self) -> Result<(), ToolFailure> {
        let target = self.clone();
        tokio::task::spawn_blocking(move || target.unchanged())
            .await
            .map_err(|error| fail("validation_failed", error.to_string()))?
    }
    fn identity_unchanged(&self) -> Result<(), ToolFailure> {
        match self.parent.open_at(
            &self.name,
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY,
            0,
        ) {
            Ok(file) => {
                let metadata = file.metadata().map_err(io)?;
                let Some(original) = &self.original else {
                    return Err(conflict());
                };
                if !metadata.is_file()
                    || FileStamp::from(&metadata)? != original.stamp
                    || metadata.permissions().mode() != original.permissions.mode()
                {
                    return Err(conflict());
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && self.original.is_none() => {}
            Err(_) => return Err(conflict()),
        }
        Ok(())
    }
    async fn lock(&self) -> Result<Lease, ToolFailure> {
        // Retain lock inodes outside the worktree so edits do not pollute Git patches.
        let root =
            std::env::temp_dir().join(format!("aicoder-file-locks-{}", unsafe { libc::geteuid() }));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io(error)),
        }
        let parent = Parent::open(&root)?;
        let metadata = parent.file.metadata().map_err(io)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(fail(
                "lock_unavailable",
                "File lock directory is not private and owned by this user",
            ));
        }
        let name = CString::new(&key(&self.path)[..2]).expect("hex lock stripe");
        let file = parent
            .open_at(&name, libc::O_RDWR | libc::O_CREAT, 0o600)
            .map_err(io)?;
        let metadata = file.metadata().map_err(io)?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(fail(
                "lock_unavailable",
                "File lock is not a private regular file",
            ));
        }
        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(Lease(file)),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(error) => return Err(io(error)),
            }
        }
    }
}
pub(super) struct FilePlan {
    pub target: Target,
    pub updated: Vec<u8>,
    pub output: Value,
    pub preview: ToolPreview,
}
impl FilePlan {
    pub fn new(
        context: &ToolContext,
        target: Target,
        updated: Vec<u8>,
        mut output: Value,
    ) -> Result<Self, ToolFailure> {
        if updated.len() > MAX_FILE_BYTES {
            return Err(fail(
                "file_too_large",
                "Replacement exceeds the 4 MiB mutation limit",
            ));
        }
        if target
            .original
            .as_ref()
            .is_some_and(|original| original.bytes.as_ref() == updated.as_slice())
        {
            return Err(fail("no_change", "Replacement does not change the file"));
        }
        let label = WorkspacePaths::from_context(context).relative_path(&target.path);
        let before = target
            .original
            .as_ref()
            .map_or(&[][..], |s| s.bytes.as_ref());
        let preview = preview(&label, before, &updated, target.original.is_none());
        output["path"] = json!(label);
        output["revision"] = json!("v1:0000000000000000");
        if output.to_string().len() > context.max_output_bytes() {
            return Err(fail(
                "output_limit_too_small",
                "Output budget cannot hold the mutation receipt",
            ));
        }
        Ok(Self {
            target,
            updated,
            output,
            preview,
        })
    }
    async fn commit(mut self, context: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        if self.output.to_string().len() > context.max_output_bytes() {
            return Err(fail(
                "output_limit_too_small",
                "Output budget cannot hold the mutation receipt",
            ));
        }
        let _lease = self.target.lock().await?;
        self.verify_route(context)?;
        self.target.validate_snapshot().await?;
        let name = CString::new(format!(".aicoder-{}.tmp", uuid::Uuid::new_v4())).expect("UUID");
        let file = self
            .target
            .parent
            .open_at(&name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600)
            .map_err(io)?;
        let temporary = Temporary {
            parent: self.target.parent.clone(),
            name,
        };
        // Creation is synchronous; cancellation cannot leave a later-created temporary path.
        // Dropped Tokio writes can finish only on this private, unlinked staging inode.
        let mut file = tokio::fs::File::from_std(file);
        file.write_all(&self.updated).await.map_err(io)?;
        file.flush().await.map_err(io)?;
        let mode = self
            .target
            .original
            .as_ref()
            .map_or(0o600, |s| s.permissions.mode() & 0o777);
        file.set_permissions(Permissions::from_mode(mode))
            .await
            .map_err(io)?;
        file.sync_all().await.map_err(io)?;
        let published_revision = revision(&FileStamp::from(&file.metadata().await.map_err(io)?)?);
        self.verify_route(context)?;
        self.target.validate_snapshot().await?;
        self.verify_route(context)?;
        self.target.identity_unchanged()?;
        // No await between final validation and publication. Participating writers hold the
        // same stable lock; arbitrary editors still require best-effort conflict checks.
        let fd = self.target.parent.file.as_raw_fd();
        let result = unsafe {
            if self.target.original.is_none() {
                libc::linkat(
                    fd,
                    temporary.name.as_ptr(),
                    fd,
                    self.target.name.as_ptr(),
                    0,
                )
            } else {
                libc::renameat(fd, temporary.name.as_ptr(), fd, self.target.name.as_ptr())
            }
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                conflict()
            } else {
                io(error)
            });
        }
        drop(temporary);
        self.output["revision"] = json!(published_revision);
        if let Err(error) = self.target.parent.file.sync_all() {
            return Err(fail("durability_uncertain", format!("File was published but directory synchronization failed: {error}; inspect before retrying"))
                .with_details(json!({"committed":true,"durability":"uncertain","receipt":self.output})));
        }
        Ok(ToolSuccess::new(self.output))
    }
    fn verify_route(&self, context: &ToolContext) -> Result<(), ToolFailure> {
        let resolved = WorkspacePaths::from_context(context)
            .resolve_for_write(&self.target.argument)
            .map_err(|_| conflict())?;
        if resolved != self.target.path {
            return Err(conflict());
        }
        self.target.parent.check()
    }
}
#[async_trait]
impl PreparedTool for FilePlan {
    fn effects(&self) -> aicoder_core::tools::PreparedToolEffects<'_> {
        aicoder_core::tools::PreparedToolEffects::FileMutation {
            paths: std::slice::from_ref(&self.target.path),
        }
    }
    fn preview(&self) -> ToolPreview {
        self.preview.clone()
    }
    async fn execute(self: Box<Self>, context: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        tokio::time::timeout(context.tool_timeout(), self.commit(context))
            .await
            .map_err(|_| fail("timeout", "File mutation exceeded its time limit"))?
    }
}
fn preview(path: &str, before: &[u8], after: &[u8], created: bool) -> ToolPreview {
    let summary = format!(
        "{} {:?}: {} -> {} bytes",
        if created { "Create" } else { "Replace" },
        path,
        before.len(),
        after.len()
    );
    let (summary, summary_cut) = truncate_utf8(&summary, 2048);
    let mut preview = ToolPreview {
        summary,
        body: "Diff omitted: binary text or preview input exceeds 256 KiB / 8192 lines. The complete replacement remains the prepared operation.".into(),
        format: "unified_diff".into(),
        truncated: true,
    };
    if let (Ok(old), Ok(new)) = (std::str::from_utf8(before), std::str::from_utf8(after))
        && old.len() + new.len() <= 256 * 1024
        && old.lines().count() + new.lines().count() <= 8192
    {
        let diff = similar::TextDiff::configure()
            .timeout(Duration::from_millis(100))
            .diff_lines(old, new);
        let mut bytes = LimitedWriter {
            bytes: Vec::new(),
            limit: 16 * 1024,
        };
        let old_name = if created {
            "/dev/null".into()
        } else {
            format!("{:?}", format!("a/{path}"))
        };
        let new_name = format!("{:?}", format!("b/{path}"));
        let result = diff
            .unified_diff()
            .context_radius(3)
            .header(&old_name, &new_name)
            .to_writer(&mut bytes);
        preview.body = String::from_utf8_lossy(&bytes.bytes).into_owned();
        preview.truncated = result.is_err() || summary_cut;
    }
    while serde_json::to_vec(&preview).expect("preview").len()
        > aicoder_core::tools::MAX_TOOL_PREVIEW_BYTES
    {
        preview.body = truncate_utf8(&preview.body, preview.body.len() / 2).0;
        preview.truncated = true;
    }
    preview
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = bytes.len().min(self.limit - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..count]);
        if count == 0 && !bytes.is_empty() {
            return Err(std::io::Error::other("preview limit"));
        }
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
