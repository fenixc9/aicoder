use super::manifest::{MAX_MANIFEST_BYTES, name};
use super::*;
use crate::resources::reader::{Root, SealedFile, hex, sha256};
use aicoder_core::TurnExecutionContext;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
const MAX_INDEX_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct PackageLimits {
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_package_bytes: usize,
    pub max_objects: usize,
    pub max_store_bytes: u64,
}
impl Default for PackageLimits {
    fn default() -> Self {
        Self {
            max_files: 1024,
            max_file_bytes: 16 * 1024 * 1024,
            max_package_bytes: 64 * 1024 * 1024,
            max_objects: 256,
            max_store_bytes: 512 * 1024 * 1024,
        }
    }
}
impl PackageLimits {
    fn validate(&self) -> Result<()> {
        ensure!(
            (1..=1024).contains(&self.max_files)
                && (1..=32 * 1024 * 1024).contains(&self.max_file_bytes)
                && (1..=128 * 1024 * 1024).contains(&self.max_package_bytes)
                && (1..=512).contains(&self.max_objects)
                && self.max_store_bytes >= self.max_package_bytes as u64
                && self.max_store_bytes <= 4 * 1024 * 1024 * 1024,
            "invalid package storage limits"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageSource {
    Local {
        path: PathBuf,
    },
    Git {
        url: String,
        requested_revision: String,
        commit: String,
        archive_sha256: String,
    },
    Npm {
        registry: String,
        package: String,
        version: String,
        integrity: String,
        archive_sha256: String,
    },
}
impl PackageSource {
    fn validate(&self) -> Result<()> {
        let digest = |v: &str| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit());
        let label = |v: &str| max_label(v, 256);
        match self {
            Self::Local { path } => ensure!(
                path.is_absolute() && path.as_os_str().as_encoded_bytes().len() <= 4096,
                "invalid local package provenance"
            ),
            Self::Git {
                url,
                requested_revision,
                commit,
                archive_sha256,
            } => {
                provenance_url(url)?;
                ensure!(
                    label(requested_revision)
                        && (commit.len() == 40 || commit.len() == 64)
                        && commit.bytes().all(|b| b.is_ascii_hexdigit())
                        && digest(archive_sha256),
                    "invalid Git package provenance"
                );
            }
            Self::Npm {
                registry,
                package,
                version,
                integrity,
                archive_sha256,
            } => {
                provenance_url(registry)?;
                ensure!(
                    max_label(package, 214)
                        && max_label(version, 128)
                        && integrity.len() <= 4096
                        && !integrity.is_empty()
                        && integrity.bytes().all(|b| b.is_ascii_graphic())
                        && digest(archive_sha256),
                    "invalid npm package provenance"
                );
            }
        }
        Ok(())
    }
}
fn max_label(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && value.bytes().all(|b| b.is_ascii_graphic())
}
fn provenance_url(value: &str) -> Result<()> {
    ensure!(
        value.len() <= 4096,
        "package provenance URL budget exceeded"
    );
    let url =
        url::Url::parse(value).map_err(|_| anyhow::anyhow!("invalid package provenance URL"))?;
    let local = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name == "localhost",
        _ => false,
    };
    ensure!(
        url.scheme() == "https" || url.scheme() == "http" && local,
        "invalid package provenance transport"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "package provenance cannot contain URL credentials or capability parameters"
    );
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRecord {
    pub pin: PackagePin,
    pub source: PackageSource,
    pub bytes: u64,
    pub execution_allowed: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackageIndex {
    pub version: u32,
    pub revision: u64,
    pub installed: BTreeMap<String, PackageRecord>,
    pub active: BTreeMap<String, String>,
    pub enabled: BTreeSet<String>,
}
impl PackageIndex {
    fn validate(&self) -> Result<()> {
        ensure!(
            (self.version == 1 || self.version == 2)
                && self.installed.len() <= 512
                && self.active.len() <= 64
                && self.enabled.len() <= 32,
            "invalid package registry version/count"
        );
        for (digest, record) in &self.installed {
            record.pin.validate()?;
            record.source.validate()?;
            ensure!(
                self.version == 2 || matches!(record.source, PackageSource::Local { .. }),
                "remote package provenance requires registry schema 2"
            );
            ensure!(
                digest == &record.pin.sha256 && record.bytes <= 128 * 1024 * 1024,
                "invalid installed package record"
            );
        }
        for (id, digest) in &self.active {
            name(id)?;
            ensure!(
                self.installed.get(digest).is_some_and(|r| r.pin.id == *id),
                "active package pin is missing"
            );
        }
        ensure!(
            self.enabled.iter().all(|id| self.active.contains_key(id)),
            "enabled package has no active pin"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Copy)]
pub struct InstallOptions {
    pub allow_execution: bool,
    pub enable: bool,
}
impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            allow_execution: false,
            enable: true,
        }
    }
}
pub struct PackageStore {
    pub(crate) root: PathBuf,
    pub(crate) handle: Root,
    pub(crate) limits: PackageLimits,
}
impl std::fmt::Debug for PackageStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageStore")
            .field("root", &self.root)
            .field("limits", &self.limits)
            .finish()
    }
}
pub(crate) struct FileRecord {
    pub path: String,
    pub sealed: SealedFile,
    pub executable: bool,
}
fn private(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        !meta.file_type().is_symlink() && meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
        "package storage must be an owned directory"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
pub(crate) struct Stage(pub(crate) PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl PackageStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::with_limits(path, PackageLimits::default())
    }
    pub fn with_limits(path: impl AsRef<Path>, limits: PackageLimits) -> Result<Self> {
        limits.validate()?;
        fs::create_dir_all(path.as_ref())?;
        private(path.as_ref())?;
        let root = path.as_ref().canonicalize()?;
        for name in ["objects", "staging"] {
            let path = root.join(name);
            fs::create_dir_all(&path)?;
            private(&path)?;
        }
        let handle = Root::open(&root)?;
        Ok(Self {
            root,
            handle,
            limits,
        })
    }
    pub(crate) fn existing(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(meta) => {
                ensure!(
                    meta.is_dir()
                        && !meta.file_type().is_symlink()
                        && meta.uid() == unsafe { libc::geteuid() }
                        && meta.mode() & 0o077 == 0,
                    "invalid package store root"
                );
                Ok(Some(Self {
                    root: path.canonicalize()?,
                    handle: Root::open(path)?,
                    limits: Default::default(),
                }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn lock(&self, exclusive: bool) -> Result<Lock> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.root.join("registry.lock"))?;
        ensure!(
            file.metadata()?.is_file(),
            "package registry lock is not a regular file"
        );
        if exclusive {
            FileExt::lock_exclusive(&file)?;
        } else {
            FileExt::lock_shared(&file)?;
        }
        Ok(Lock(file))
    }
    pub fn index(&self) -> Result<PackageIndex> {
        let _lock = self.lock(false)?;
        self.read_index()
    }
    pub(crate) fn read_index(&self) -> Result<PackageIndex> {
        let bytes = match self
            .handle
            .read(Path::new("registry.json"), MAX_INDEX_BYTES)
        {
            Ok(bytes) => bytes,
            Err(error)
                if error.chain().any(|e| {
                    e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                }) =>
            {
                return Ok(PackageIndex {
                    version: 1,
                    ..Default::default()
                });
            }
            Err(error) => return Err(error),
        };
        let index: PackageIndex = serde_json::from_slice(&bytes)?;
        index.validate()?;
        Ok(index)
    }
    fn publish(&self, mut index: PackageIndex) -> Result<()> {
        index.revision = index
            .revision
            .checked_add(1)
            .context("package registry revision exhausted")?;
        index.validate()?;
        let bytes = serde_json::to_vec(&index)?;
        ensure!(
            bytes.len() <= MAX_INDEX_BYTES,
            "package registry byte budget exceeded"
        );
        let path = self
            .root
            .join(format!(".registry-{}", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let result = (|| -> Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&path, self.root.join("registry.json"))?;
            File::open(&self.root)?.sync_all().context("package registry is visible but directory durability failed; inspect registry before retrying")?;
            Ok(())
        })();
        let _ = fs::remove_file(path);
        result
    }
    pub fn inspect(source: impl AsRef<Path>) -> Result<PackageManifest> {
        let root = Root::open(source.as_ref())?;
        PackageManifest::parse(&root.read(Path::new(MANIFEST_FILE), MAX_MANIFEST_BYTES)?)
    }
    pub fn install_local(
        &self,
        source: impl AsRef<Path>,
        options: InstallOptions,
        context: &TurnExecutionContext,
    ) -> Result<PackageRecord> {
        let source = Root::open(source.as_ref())?;
        self.import(
            &source,
            PackageSource::Local {
                path: source.path.clone(),
            },
            options,
            context,
        )
    }
    pub(crate) fn import(
        &self,
        source: &Root,
        origin: PackageSource,
        options: InstallOptions,
        context: &TurnExecutionContext,
    ) -> Result<PackageRecord> {
        origin.validate()?;
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let raw = source.read(Path::new(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
        let manifest = PackageManifest::parse(&raw)?;
        ensure!(
            !manifest.permissions.execution || options.allow_execution,
            "package declares executable code; explicit allow-execution consent is required for this content pin"
        );
        ensure!(
            manifest.files.len() <= self.limits.max_files,
            "package file count limit exceeded"
        );
        let _lock = self.lock(true)?;
        let mut index = self.read_index()?;
        for dependency in &manifest.dependencies {
            ensure!(
                index
                    .installed
                    .get(&dependency.sha256)
                    .is_some_and(|r| r.pin == *dependency),
                "exact package dependency is not installed/trusted"
            );
        }
        let stage = Stage(
            self.root
                .join("staging")
                .join(uuid::Uuid::new_v4().to_string()),
        );
        fs::create_dir(&stage.0)?;
        private(&stage.0)?;
        let mut records = Vec::new();
        let mut total = 0u64;
        let mut paths = manifest.files.clone();
        paths.push(MANIFEST_FILE.into());
        paths.sort();
        for path in paths {
            if context.is_cancelled() {
                return Err(context.error().into());
            }
            let meta = source.metadata(Path::new(&path))?;
            ensure!(meta.is_file(), "package payload is not a regular file");
            let bytes = if path == MANIFEST_FILE {
                raw.clone()
            } else {
                source.read(Path::new(&path), self.limits.max_file_bytes)?
            };
            total = total
                .checked_add(bytes.len() as u64)
                .context("package byte overflow")?;
            ensure!(
                total <= self.limits.max_package_bytes as u64,
                "package byte budget exceeded"
            );
            let executable = path != MANIFEST_FILE && meta.mode() & 0o111 != 0;
            let destination = stage.0.join(&path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(if executable { 0o700 } else { 0o600 })
                .custom_flags(libc::O_NOFOLLOW)
                .open(destination)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            records.push(FileRecord {
                path,
                sealed: SealedFile {
                    bytes: bytes.len() as u64,
                    sha256: sha256(&bytes),
                },
                executable,
            });
        }
        super::snapshot::validate_resources(&Root::open(&stage.0)?, &manifest)?;
        sync_directories(&stage.0)?;
        let digest = tree_digest(&records);
        let pin = PackagePin {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            sha256: digest.clone(),
        };
        let destination = self.root.join("objects").join(&digest);
        if destination.exists() {
            self.object(&pin)?;
        } else {
            self.admit(total)?;
            fs::rename(&stage.0, &destination)?;
            File::open(self.root.join("objects"))?.sync_all()?;
        }
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let record = PackageRecord {
            pin: pin.clone(),
            source: origin,
            bytes: total,
            execution_allowed: options.allow_execution,
        };
        ensure!(
            index.installed.len() < 512 || index.installed.contains_key(&digest),
            "installed package revision count exceeded"
        );
        if !matches!(record.source, PackageSource::Local { .. }) {
            index.version = 2;
        }
        index.installed.insert(digest.clone(), record.clone());
        index.active.insert(pin.id.clone(), digest);
        if options.enable {
            index.enabled.insert(pin.id.clone());
        } else {
            index.enabled.remove(&pin.id);
        }
        PackageSnapshot::resolve(self, &index, None, Some(std::slice::from_ref(&pin)))?;
        PackageSnapshot::resolve(self, &index, None, None)?;
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        self.publish(index)?;
        Ok(record)
    }
    fn admit(&self, additional: u64) -> Result<()> {
        let mut count = 0usize;
        let mut bytes = 0u64;
        for entry in fs::read_dir(self.root.join("objects"))? {
            let entry = entry?;
            count += 1;
            ensure!(
                count < self.limits.max_objects,
                "package object count budget exceeded"
            );
            let mut entries = 0;
            bytes = bytes
                .checked_add(disk_size(&entry.path(), 0, &mut entries)?)
                .context("package store size overflow")?;
        }
        ensure!(
            bytes.saturating_add(additional) <= self.limits.max_store_bytes,
            "package store disk budget exceeded"
        );
        Ok(())
    }
    pub fn enable(&self, id: &str, enabled: bool) -> Result<()> {
        name(id)?;
        let _lock = self.lock(true)?;
        let mut index = self.read_index()?;
        ensure!(index.active.contains_key(id), "package not installed");
        if enabled {
            index.enabled.insert(id.into());
        } else {
            index.enabled.remove(id);
        }
        PackageSnapshot::resolve(self, &index, None, None)?;
        self.publish(index)
    }
    pub fn select_revision(&self, pin: &PackagePin) -> Result<()> {
        pin.validate()?;
        let _lock = self.lock(true)?;
        let mut index = self.read_index()?;
        ensure!(
            index
                .installed
                .get(&pin.sha256)
                .is_some_and(|r| r.pin == *pin),
            "package revision is not trusted/installed"
        );
        self.object(pin)?;
        index.active.insert(pin.id.clone(), pin.sha256.clone());
        PackageSnapshot::resolve(self, &index, None, None)?;
        self.publish(index)
    }
    pub fn uninstall(&self, id: &str) -> Result<()> {
        name(id)?;
        let _lock = self.lock(true)?;
        let mut index = self.read_index()?;
        ensure!(index.active.contains_key(id), "package not installed");
        let roots: Vec<String> = index
            .enabled
            .iter()
            .filter(|other| other.as_str() != id)
            .cloned()
            .collect();
        let retained = PackageSnapshot::resolve(self, &index, Some(&roots), None)?;
        ensure!(
            retained.get(id).is_none(),
            "enabled package depends on this package; disable it first"
        );
        index.enabled.remove(id);
        index.active.remove(id);
        index.installed.retain(|_, record| record.pin.id != id);
        self.publish(index)
    }
    pub fn snapshot(
        &self,
        selected: Option<&[String]>,
        pins: Option<&[PackagePin]>,
    ) -> Result<PackageSnapshot> {
        let _lock = self.lock(false)?;
        let index = self.read_index()?;
        PackageSnapshot::resolve(self, &index, selected, pins)
    }
}
pub(crate) fn tree_digest(records: &[FileRecord]) -> String {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(b"aicoder-package-v1\0");
    for record in records {
        hash.update(&(record.path.len() as u64).to_le_bytes());
        hash.update(record.path.as_bytes());
        hash.update(&record.sealed.bytes.to_le_bytes());
        hash.update(&[u8::from(record.executable)]);
        hash.update(record.sealed.sha256.as_bytes());
    }
    hex(hash.finish().as_ref())
}
fn disk_size(path: &Path, depth: usize, entries: &mut usize) -> Result<u64> {
    *entries += 1;
    ensure!(
        *entries <= 8192 && depth <= 128,
        "package object scan budget exceeded"
    );
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        !meta.file_type().is_symlink(),
        "package object contains a link"
    );
    if meta.is_file() {
        return Ok(meta.len());
    }
    ensure!(meta.is_dir(), "package object has a special file");
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        total = total
            .checked_add(disk_size(&entry?.path(), depth + 1, entries)?)
            .context("package disk size overflow")?;
    }
    Ok(total)
}

fn sync_directories(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_directories(&entry.path())?;
        }
    }
    File::open(path)?.sync_all()?;
    Ok(())
}
