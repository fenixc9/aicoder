use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};
/// Pin a directory and open every relative component without following links.
#[derive(Clone)]
pub(super) struct Root {
    pub path: PathBuf,
    handle: Arc<File>,
}
impl Root {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        ensure!(path.is_dir(), "resource root is not a directory");
        let file = File::open(&path)?;
        Ok(Self {
            path,
            handle: Arc::new(file),
        })
    }
    pub fn directory(&self, relative: &Path) -> Result<Self> {
        let file = self.open_relative(relative)?;
        ensure!(
            file.metadata()?.is_dir(),
            "resource path is not a directory"
        );
        Ok(Self {
            path: self.path.join(relative),
            handle: Arc::new(file),
        })
    }
    pub fn read_page(
        &self,
        relative: &Path,
        offset: u64,
        limit: usize,
    ) -> Result<(String, u64, bool)> {
        use std::io::{Seek, SeekFrom};
        let mut file = self.open_relative(relative)?;
        ensure!(file.metadata()?.is_file(), "resource is not a regular file");
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        Read::take(&mut file, limit.saturating_add(1) as u64).read_to_end(&mut bytes)?;
        let more = bytes.len() > limit;
        bytes.truncate(limit);
        let length = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(error) if more && error.error_len().is_none() => error.valid_up_to(),
            Err(_) => anyhow::bail!("resource page is not UTF-8 or starts inside a code point"),
        };
        ensure!(
            length > 0 || bytes.is_empty(),
            "resource page cannot fit a UTF-8 code point"
        );
        bytes.truncate(length);
        let next = offset
            .checked_add(length as u64)
            .context("resource offset overflow")?;
        Ok((
            String::from_utf8(bytes).expect("validated UTF-8"),
            next,
            more,
        ))
    }
    pub fn read(&self, relative: &Path, limit: usize) -> Result<Vec<u8>> {
        let mut file = self.open_relative(relative)?;
        ensure!(file.metadata()?.is_file(), "resource is not a regular file");
        let mut bytes = Vec::new();
        Read::take(&mut file, limit.saturating_add(1) as u64).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= limit, "resource exceeds its file byte limit");
        Ok(bytes)
    }
    fn open_relative(&self, relative: &Path) -> Result<File> {
        ensure!(
            relative.as_os_str().as_encoded_bytes().len() <= 4096,
            "resource path exceeds its byte limit"
        );
        let parts = relative
            .components()
            .map(|part| match part {
                Component::Normal(value) => Ok(value),
                _ => anyhow::bail!("resource path must be relative and cannot traverse parents"),
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(!parts.is_empty(), "resource path is empty");
        ensure!(parts.len() <= 128, "resource path has too many components");
        #[cfg(unix)]
        {
            use std::{
                ffi::CString,
                os::{
                    fd::{AsRawFd, FromRawFd},
                    unix::ffi::OsStrExt,
                },
            };
            let mut current = self.handle.try_clone()?;
            for (index, part) in parts.iter().enumerate() {
                let name = CString::new(part.as_bytes()).context("invalid resource path")?;
                let mut flags = libc::O_RDONLY
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_NOCTTY;
                if index + 1 < parts.len() {
                    flags |= libc::O_DIRECTORY;
                }
                let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("resource path cannot be opened without following links");
                }
                current = unsafe { File::from_raw_fd(fd) };
            }
            Ok(current)
        }
        #[cfg(not(unix))]
        {
            let mut path = self.path.clone();
            for part in parts {
                path.push(part);
                ensure!(
                    !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
                    "resource links are not allowed"
                );
            }
            let path = path.canonicalize()?;
            ensure!(
                path.starts_with(&self.path),
                "resource path escaped its root"
            );
            Ok(File::open(path)?)
        }
    }
}
pub(super) fn fingerprint(bytes: &[u8]) -> String {
    let mut value = 0xcbf29ce484222325u64;
    for byte in bytes {
        value ^= u64::from(*byte);
        value = value.wrapping_mul(0x100000001b3);
    }
    format!("{value:016x}")
}
