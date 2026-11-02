//! Scoped immutable data and lease-protected metadata; never execution authority.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutputRef {
    pub path: String,
    pub bytes: usize,
    pub sha256: String,
}
/// Linux file-descriptor anchors keep even renamed directories inside the selected archive.
pub(crate) struct StorageRoot {
    directory: File,
}
fn descriptor_path(file: &File) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        PathBuf::new()
    }
}
fn open(path: &Path, directory: bool, write: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(!write).write(write).create_new(write);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(
                libc::O_NOFOLLOW | libc::O_NONBLOCK | if directory { libc::O_DIRECTORY } else { 0 },
            )
            .mode(0o600);
    }
    let file = options.open(path)?;
    ensure!(
        if directory {
            file.metadata()?.is_dir()
        } else {
            file.metadata()?.is_file()
        },
        "unexpected benchmark storage file type"
    );
    Ok(file)
}
impl StorageRoot {
    pub fn existing(root: &Path) -> Result<Self> {
        ensure!(
            cfg!(target_os = "linux"),
            "pinned benchmark archives currently require Linux /proc/self/fd"
        );
        Ok(Self {
            directory: open(root, true, false)?,
        })
    }
    pub fn create(root: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(root)?;
        Self::existing(root)
    }
    fn parent(&self, path: &str, create: bool) -> Result<(File, String)> {
        relative(path)?;
        let mut parts = Path::new(path)
            .components()
            .map(|c| match c {
                Component::Normal(p) => p.to_str().expect("validated UTF-8"),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        ensure!(parts.len() <= 16, "benchmark artifact depth budget");
        let leaf = parts.pop().expect("nonempty relative path").to_string();
        let mut directory = self.directory.try_clone()?;
        for part in parts {
            let next = descriptor_path(&directory).join(part);
            if create {
                match std::fs::create_dir(&next) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
            }
            directory = open(&next, true, false)?;
        }
        Ok((directory, leaf))
    }
    pub fn read(&self, path: &str, limit: usize) -> Result<Vec<u8>> {
        let (parent, name) = self.parent(path, false)?;
        let file = open(&descriptor_path(&parent).join(name), false, false)?;
        ensure!(
            file.metadata()?.len() <= limit as u64,
            "benchmark artifact byte limit"
        );
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= limit,
            "benchmark artifact grew beyond budget"
        );
        Ok(bytes)
    }
    pub fn write(&self, path: &str, bytes: &[u8]) -> Result<OutputRef> {
        let (parent, name) = self.parent(path, true)?;
        let mut file = open(&descriptor_path(&parent).join(name), false, true)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(OutputRef {
            path: path.into(),
            bytes: bytes.len(),
            sha256: crate::bounds::sha256(bytes),
        })
    }
    pub fn publish_json(
        &self,
        path: &str,
        value: &impl Serialize,
        limit: usize,
        replace: bool,
    ) -> Result<OutputRef> {
        let bytes = crate::bounds::encode(value, limit)?;
        let (parent, name) = self.parent(path, true)?;
        let base = descriptor_path(&parent);
        let temporary = format!(
            ".aicoder-{}.pending",
            aicoder_core::events::RunId::new().as_uuid()
        );
        let pending = base.join(&temporary);
        let mut file = open(&pending, false, true)?;
        let publish = (|| -> Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            if replace {
                std::fs::rename(&pending, base.join(&name))?;
            } else {
                std::fs::hard_link(&pending, base.join(&name))?;
                std::fs::remove_file(&pending)?;
            }
            parent.sync_all()?;
            Ok(())
        })();
        if publish.is_err() {
            let _ = std::fs::remove_file(&pending);
        }
        publish?;
        Ok(OutputRef {
            path: path.into(),
            bytes: bytes.len(),
            sha256: crate::bounds::sha256(&bytes),
        })
    }
    pub fn lease(&self) -> Result<File> {
        let path = descriptor_path(&self.directory).join(".aicoder.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .mode(0o600);
        }
        let file = options.open(path)?;
        ensure!(file.metadata()?.is_file(), "invalid output lease file");
        fs2::FileExt::try_lock_exclusive(&file)?;
        Ok(file)
    }
    pub fn exists(&self, path: &str) -> Result<bool> {
        match self.parent(path, false) {
            Ok((parent, name)) => {
                match std::fs::symlink_metadata(descriptor_path(&parent).join(name)) {
                    Ok(_) => Ok(true),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                    Err(e) => Err(e.into()),
                }
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
}
fn relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && path.len() <= 4096
            && !path.contains('\\')
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && Path::new(path)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "invalid scoped output path"
    );
    Ok(())
}
