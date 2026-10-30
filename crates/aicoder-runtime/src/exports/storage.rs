use super::*;
use std::{
    ffi::{CString, OsStr},
    fs::{self, File, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
};

/// A failed commit can already be visible if directory sync/path verification failed.
/// Callers must inspect this state instead of automatically retrying publication.
#[derive(Debug)]
pub struct CommitFailure {
    pub destination: PathBuf,
    pub visible: bool,
    pub stage: &'static str,
    pub message: String,
}
impl std::fmt::Display for CommitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Export commit {} at {} (visible={}): {}; inspect before retrying",
            self.stage,
            self.destination.display(),
            self.visible,
            self.message
        )
    }
}
impl std::error::Error for CommitFailure {}
#[derive(Clone)]
struct Parent {
    file: Arc<File>,
    path: PathBuf,
}
impl Parent {
    fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        ensure!(
            path.to_str().is_some(),
            "export destination directory must use UTF-8"
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let result = Self {
            file: Arc::new(file),
            path,
        };
        result.check()?;
        Ok(result)
    }
    fn check(&self) -> Result<()> {
        let current = fs::symlink_metadata(&self.path)?;
        let pinned = self.file.metadata()?;
        ensure!(
            current.is_dir()
                && !current.file_type().is_symlink()
                && current.dev() == pinned.dev()
                && current.ino() == pinned.ino(),
            "export destination directory changed"
        );
        Ok(())
    }
    fn create(&self, name: &CString) -> io::Result<File> {
        // SAFETY: the directory descriptor and NUL-terminated private filename remain valid.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
}
struct Stage {
    parent: Parent,
    name: CString,
}
impl Drop for Stage {
    fn drop(&mut self) {
        unsafe {
            libc::unlinkat(self.parent.file.as_raw_fd(), self.name.as_ptr(), 0);
        }
    }
}
pub(super) fn write_new(
    export: &PreparedExport,
    path: &Path,
    url: Option<String>,
    context: &TurnExecutionContext,
) -> Result<PublishedExport> {
    ensure!(path.to_str().is_some(), "export destination must use UTF-8");
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("export destination needs a filename"))?;
    let parent = Parent::open(directory)?;
    write_in(export, &parent, name, url, context)
}
fn write_in(
    export: &PreparedExport,
    parent: &Parent,
    name: &OsStr,
    url: Option<String>,
    context: &TurnExecutionContext,
) -> Result<PublishedExport> {
    write_with_hook(export, parent, name, url, context, || Ok(()))
}
fn write_with_hook(
    export: &PreparedExport,
    parent: &Parent,
    name: &OsStr,
    url: Option<String>,
    context: &TurnExecutionContext,
    after_link: impl FnOnce() -> Result<()>,
) -> Result<PublishedExport> {
    let destination = parent.path.join(name);
    let mut visible = false;
    let mut phase = "staging";
    let result = (|| -> Result<PublishedExport> {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        parent.check()?;
        let name = CString::new(name.as_bytes())?;
        let temporary = CString::new(format!(".aicoder-export-{}.part", uuid::Uuid::new_v4()))?;
        let mut file = parent.create(&temporary)?;
        let stage = Stage {
            parent: parent.clone(),
            name: temporary,
        };
        // Reserve the bounded known length where supported. ENOSPC is detected before visibility.
        let allocation = unsafe {
            libc::posix_fallocate(file.as_raw_fd(), 0, export.bytes.len() as libc::off_t)
        };
        if allocation != 0 && ![libc::EOPNOTSUPP, libc::EINVAL, libc::ENOSYS].contains(&allocation)
        {
            return Err(io::Error::from_raw_os_error(allocation).into());
        }
        for bytes in export.bytes.chunks(16384) {
            if context.is_cancelled() {
                return Err(context.error().into());
            }
            file.write_all(bytes)?;
        }
        file.sync_all()?;
        phase = "publication";
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        parent.check()?;
        // linkat is atomic and refuses existing files/symlinks; no replacement can destroy history.
        let linked = unsafe {
            libc::linkat(
                parent.file.as_raw_fd(),
                stage.name.as_ptr(),
                parent.file.as_raw_fd(),
                name.as_ptr(),
                0,
            )
        };
        if linked != 0 {
            return Err(io::Error::last_os_error().into());
        }
        visible = true;
        phase = "directory_sync";
        after_link()?;
        drop(stage);
        parent.file.sync_all()?;
        parent.check()?;
        Ok(PublishedExport {
            export: export.receipt.clone(),
            path: destination.clone(),
            url,
        })
    })();
    result.map_err(|error| {
        CommitFailure {
            destination,
            visible,
            stage: phase,
            message: error.to_string(),
        }
        .into()
    })
}
/// Concrete local publication adapter. The directory/base URL are trusted host configuration.
/// A URL receipt does not imply a server was deployed or remote content was verified.
#[derive(Clone)]
pub struct DirectoryPublisher {
    parent: Parent,
    base_url: Option<url::Url>,
}
impl DirectoryPublisher {
    pub fn new(directory: &Path, base_url: Option<&str>) -> Result<Self> {
        let base_url = base_url
            .map(|value| -> Result<url::Url> {
                let mut url = url::Url::parse(value)?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none(),
                    "publication base URL must be HTTP(S) without credentials, query or fragment"
                );
                if !url.path().ends_with('/') {
                    url.set_path(&format!("{}/", url.path()));
                }
                Ok(url)
            })
            .transpose()?;
        ensure!(
            directory.to_str().is_some(),
            "publication directory must use UTF-8"
        );
        Ok(Self {
            parent: Parent::open(directory)?,
            base_url,
        })
    }
}
struct CancelOnDrop(TurnExecutionContext);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel("Publication future dropped");
    }
}
#[async_trait::async_trait]
impl ExportPublisher for DirectoryPublisher {
    async fn publish(
        &self,
        export: &PreparedExport,
        context: TurnExecutionContext,
    ) -> Result<PublishedExport> {
        let context = context.child();
        let _guard = CancelOnDrop(context.clone());
        let filename = format!(
            "{}-r{}-{}.{}",
            export.receipt.session_id,
            export.receipt.revision,
            uuid::Uuid::new_v4(),
            export.receipt.format.extension()
        );
        let url = self
            .base_url
            .as_ref()
            .map(|base| base.join(&filename).map(String::from))
            .transpose()?;
        let export = export.clone();
        let parent = self.parent.clone();
        tokio::task::spawn_blocking(move || {
            write_in(&export, &parent, OsStr::new(&filename), url, &context)
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicoder_core::{
        model::{Message, MessageRole},
        session::SessionRepository,
    };
    #[test]
    fn directory_moving_after_visibility_is_reported_without_retry_or_partial_stage() {
        let root = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let destination = root.path().join("destination");
        fs::create_dir(&destination).unwrap();
        let moved = root.path().join("moved");
        let repo = JsonlSessionRepository::new(storage.path()).unwrap();
        let mut session = repo.create(root.path()).unwrap();
        repo.append(
            &mut session,
            Message::text(MessageRole::User, "visible complete file"),
        )
        .unwrap();
        let export = prepare(
            &repo,
            root.path(),
            &session.metadata().id,
            ExportFormat::Json,
            ExportLimits::default(),
            &TurnExecutionContext::new(),
        )
        .unwrap();
        let parent = Parent::open(&destination).unwrap();
        let failure = write_with_hook(
            &export,
            &parent,
            OsStr::new("complete.json"),
            None,
            &TurnExecutionContext::new(),
            || {
                fs::rename(&destination, &moved)?;
                fs::create_dir(&destination)?;
                Ok(())
            },
        )
        .unwrap_err();
        let error = failure.downcast_ref::<CommitFailure>().unwrap();
        assert!(error.visible);
        assert_eq!(error.stage, "directory_sync");
        assert_eq!(
            fs::read(moved.join("complete.json")).unwrap(),
            export.bytes()
        );
        assert_eq!(fs::read_dir(&moved).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    }
}
