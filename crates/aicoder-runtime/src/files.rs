use anyhow::{Context, Result, ensure};
use std::{
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};
pub(crate) fn read_bounded(
    path: &Path,
    limit: u64,
    required: bool,
) -> Result<Option<(PathBuf, Vec<u8>)>> {
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Cannot locate configuration file {}", path.display()));
        }
    };
    ensure!(
        std::fs::metadata(&canonical)?.is_file(),
        "configuration path is not a regular file: {}",
        path.display()
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let file = options.open(&canonical)?;
    ensure!(
        file.metadata()?.is_file(),
        "configuration path is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "configuration file exceeds its byte limit: {}",
        canonical.display()
    );
    Ok(Some((canonical, bytes)))
}
