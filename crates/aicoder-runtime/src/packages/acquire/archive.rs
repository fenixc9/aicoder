use super::*;

struct BoundedReader<'a> {
    inner: Box<dyn Read + 'a>,
    remaining: usize,
    context: &'a TurnExecutionContext,
    started: Instant,
    timeout: Duration,
}
impl Read for BoundedReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        check(self.context, self.started, self.timeout).map_err(std::io::Error::other)?;
        let length = output.len().min(self.remaining.saturating_add(1));
        let read = self.inner.read(&mut output[..length])?;
        if read > self.remaining {
            return Err(std::io::Error::other(
                "package unpacked byte budget exhausted",
            ));
        }
        self.remaining -= read;
        Ok(read)
    }
}
pub(super) fn extract(
    bytes: &[u8],
    gzip: bool,
    destination: &Path,
    limits: PackageAcquireLimits,
    context: &TurnExecutionContext,
    started: Instant,
) -> Result<()> {
    let input: Box<dyn Read> = if gzip {
        Box::new(flate2::read::MultiGzDecoder::new(Cursor::new(bytes)))
    } else {
        Box::new(Cursor::new(bytes))
    };
    let reader = BoundedReader {
        inner: input,
        remaining: limits.max_unpacked_bytes,
        context,
        started,
        timeout: limits.timeout,
    };
    let mut archive = tar::Archive::new(reader);
    let mut seen = BTreeSet::new();
    let mut nodes = BTreeSet::new();
    let mut long_name: Option<String> = None;
    let mut pax_path: Option<String> = None;
    let mut pax_size: Option<u64> = None;
    let mut pending = false;
    // Raw iteration admits every extension header before allocation and never parses sparse
    // logical files. Only bounded path/size metadata is interpreted explicitly below.
    for (index, entry) in archive.entries()?.raw(true).enumerate() {
        ensure!(
            index < limits.max_entries,
            "package archive entry budget exhausted"
        );
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_gnu_longname() {
            ensure!(
                long_name.is_none() && entry.size() <= 4097,
                "invalid GNU path metadata"
            );
            let mut value = Vec::new();
            entry.read_to_end(&mut value)?;
            if value.last() == Some(&0) {
                value.pop();
            }
            long_name = Some(
                String::from_utf8(value)
                    .map_err(|_| anyhow::anyhow!("invalid GNU path encoding"))?,
            );
            pending = true;
            continue;
        }
        if kind.is_pax_local_extensions() || kind.is_pax_global_extensions() {
            ensure!(entry.size() <= 65536, "PAX metadata byte budget exhausted");
            let global = kind.is_pax_global_extensions();
            let mut keys = BTreeSet::new();
            for extension in entry
                .pax_extensions()?
                .ok_or_else(|| anyhow::anyhow!("PAX metadata missing"))?
            {
                let extension = extension?;
                let key = extension.key()?;
                let value = extension.value()?;
                ensure!(
                    keys.insert(key.to_owned()) && !key.starts_with("GNU.sparse"),
                    "ambiguous or sparse PAX metadata"
                );
                match key {
                    "path" => {
                        ensure!(
                            !global && pax_path.is_none() && value.len() <= 4096,
                            "ambiguous PAX path"
                        );
                        pax_path = Some(value.into());
                    }
                    "size" => {
                        ensure!(!global && pax_size.is_none(), "ambiguous PAX size");
                        pax_size = Some(
                            value
                                .parse()
                                .map_err(|_| anyhow::anyhow!("invalid PAX size"))?,
                        );
                    }
                    "linkpath" => anyhow::bail!("package archive links are forbidden"),
                    _ => {} // Inert ownership/time/comment metadata is never applied to host files.
                }
            }
            pending |= !global;
            continue;
        }
        ensure!(
            kind == tar::EntryType::Regular || kind == tar::EntryType::Directory,
            "package archive links/special files are forbidden"
        );
        if kind == tar::EntryType::Directory {
            ensure!(entry.size() == 0, "directory archive entry contains data");
        }
        ensure!(
            pax_size.is_none_or(|size| size == entry.size()),
            "conflicting PAX/header file size"
        );
        pax_size = None;
        if let (Some(gnu), Some(pax)) = (&long_name, &pax_path) {
            ensure!(gnu == pax, "conflicting archive path metadata");
        }
        let raw = entry.path_bytes();
        let path = std::str::from_utf8(&raw)
            .map_err(|_| anyhow::anyhow!("package archive path must be UTF-8"))?;
        let name = pax_path
            .take()
            .or_else(|| long_name.take())
            .unwrap_or_else(|| path.into());
        long_name = None;
        pending = false;
        let path = name.as_str();
        let path = path.strip_suffix('/').unwrap_or(path);
        if path == "package" && kind == tar::EntryType::Directory {
            continue;
        }
        let relative = path
            .strip_prefix("package/")
            .ok_or_else(|| anyhow::anyhow!("package archive root prefix is invalid"))?;
        ensure!(
            !relative.is_empty()
                && relative.len() <= 4096
                && !relative.contains('\\')
                && !relative.chars().any(char::is_control)
                && relative.split('/').count() <= 64
                && relative
                    .split('/')
                    .all(|p| !p.is_empty() && p != "." && p != ".."),
            "invalid package archive path"
        );
        ensure!(
            seen.insert(relative.to_owned()),
            "duplicate package archive entry"
        );
        let target = destination.join(relative);
        let components = relative.split('/').collect::<Vec<_>>();
        let directories = if kind == tar::EntryType::Directory {
            components.len()
        } else {
            components.len() - 1
        };
        for length in 1..=directories {
            let name = components[..length].join("/");
            if nodes.insert(name.clone()) {
                ensure!(
                    nodes.len() <= limits.max_entries,
                    "package extracted node budget exhausted"
                );
                fs::create_dir(destination.join(name))?;
            }
        }
        if kind == tar::EntryType::Directory {
            continue;
        }
        ensure!(
            nodes.insert(relative.to_owned()) && nodes.len() <= limits.max_entries,
            "package extracted node collision/budget exhausted"
        );
        ensure!(
            entry.size() <= 32 * 1024 * 1024,
            "package archive file size budget exhausted"
        );
        let executable = entry.header().mode()? & 0o111 != 0;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if executable { 0o700 } else { 0o600 })
            .custom_flags(libc::O_NOFOLLOW)
            .open(target)?;
        let mut chunk = [0u8; 65536];
        loop {
            let count = entry.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            output.write_all(&chunk[..count])?;
        }
    }
    ensure!(!pending, "archive path metadata has no following entry");
    let mut tail = archive.into_inner();
    let mut chunk = [0u8; 65536];
    loop {
        let count = tail.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        ensure!(
            chunk[..count].iter().all(|b| *b == 0),
            "package archive contains trailing data"
        );
    }
    Ok(())
}
