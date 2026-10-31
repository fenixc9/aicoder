use super::*;
use super::{
    manifest::{MAX_MANIFEST_BYTES, relative},
    store::{FileRecord, tree_digest},
};
use crate::resources::reader::{Root, SealedFile, sha256};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
#[derive(Clone)]
pub struct PackageObject {
    pub pin: PackagePin,
    pub manifest: PackageManifest,
    pub execution_allowed: bool,
    pub(crate) root: Root,
}
impl std::fmt::Debug for PackageObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageObject")
            .field("pin", &self.pin)
            .field("execution_allowed", &self.execution_allowed)
            .finish()
    }
}
impl PackageObject {
    pub fn root(&self) -> &Path {
        &self.root.path
    }
    pub fn read_asset(&self, path: &str, limit: usize) -> Result<Vec<u8>> {
        relative(path)?;
        ensure!(
            limit <= 32 * 1024 * 1024,
            "package asset read budget exceeded"
        );
        self.root.read(Path::new(path), limit)
    }
}
#[derive(Clone, Default, Debug)]
pub struct PackageSnapshot {
    pub(crate) objects: Vec<PackageObject>,
    pub registry_revision: u64,
}
impl PackageSnapshot {
    pub fn packages(&self) -> &[PackageObject] {
        &self.objects
    }
    pub fn pins(&self) -> Vec<PackagePin> {
        self.objects.iter().map(|p| p.pin.clone()).collect()
    }
    pub fn get(&self, id: &str) -> Option<&PackageObject> {
        self.objects.iter().find(|p| p.pin.id == id)
    }
    pub(crate) fn resolve(
        store: &PackageStore,
        index: &PackageIndex,
        selected: Option<&[String]>,
        pins: Option<&[PackagePin]>,
    ) -> Result<Self> {
        let roots = if let Some(pins) = pins {
            ensure!(pins.len() <= 32, "package pin selection exceeded");
            pins.to_vec()
        } else {
            let names = selected
                .map(<[String]>::to_vec)
                .unwrap_or_else(|| index.enabled.iter().cloned().collect());
            ensure!(names.len() <= 32, "package root selection exceeded");
            names
                .iter()
                .map(|id| {
                    index
                        .active
                        .get(id)
                        .and_then(|hash| index.installed.get(hash))
                        .map(|record| record.pin.clone())
                        .ok_or_else(|| anyhow::anyhow!("selected package is not installed"))
                })
                .collect::<Result<Vec<_>>>()?
        };
        let mut result = Self {
            objects: Vec::new(),
            registry_revision: index.revision,
        };
        let mut visited = BTreeMap::new();
        let mut active = BTreeSet::new();
        for pin in roots {
            result.visit(store, index, &pin, &mut visited, &mut active)?;
        }
        Ok(result)
    }
    fn visit(
        &mut self,
        store: &PackageStore,
        index: &PackageIndex,
        pin: &PackagePin,
        visited: &mut BTreeMap<String, String>,
        active: &mut BTreeSet<String>,
    ) -> Result<()> {
        pin.validate()?;
        if let Some(hash) = visited.get(&pin.id) {
            ensure!(
                hash == &pin.sha256,
                "package graph selects conflicting revisions"
            );
            return Ok(());
        }
        ensure!(
            self.objects.len() + active.len() < 32 && active.insert(pin.id.clone()),
            "package graph cycle/count budget exceeded"
        );
        let record = index
            .installed
            .get(&pin.sha256)
            .filter(|record| record.pin == *pin)
            .context("saved/selected package pin is not installed or trusted")?;
        let mut object = store.object(pin)?;
        object.execution_allowed = record.execution_allowed;
        for dependency in &object.manifest.dependencies {
            self.visit(store, index, dependency, visited, active)?;
        }
        active.remove(&pin.id);
        visited.insert(pin.id.clone(), pin.sha256.clone());
        self.objects.push(object);
        Ok(())
    }
    pub fn validate_trust(&self, store: &PackageStore) -> Result<()> {
        let index = store.index()?;
        for object in &self.objects {
            let record = index
                .installed
                .get(&object.pin.sha256)
                .filter(|record| record.pin == object.pin)
                .context("package trust was revoked; reload the host snapshot")?;
            ensure!(
                !object.manifest.permissions.execution || record.execution_allowed,
                "package execution consent was revoked"
            );
            store.object(&object.pin)?;
        }
        Ok(())
    }
}
impl PackageStore {
    pub(crate) fn object(&self, pin: &PackagePin) -> Result<PackageObject> {
        pin.validate()?;
        let root = self
            .handle
            .directory(Path::new("objects"))?
            .directory(Path::new(&pin.sha256))?;
        let raw = root.read(Path::new(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
        let manifest = PackageManifest::parse(&raw)?;
        ensure!(
            manifest.id == pin.id
                && manifest.version == pin.version
                && manifest.files.len() <= self.limits.max_files,
            "package object identity/count mismatch"
        );
        let mut paths = manifest.files.clone();
        paths.push(MANIFEST_FILE.into());
        paths.sort();
        let expected: BTreeSet<_> = paths.iter().cloned().collect();
        let mut actual = BTreeSet::new();
        let mut entries = 0;
        list_files(&root.path, &root.path, 0, &mut entries, &mut actual)?;
        ensure!(
            actual == expected,
            "package object contains undeclared or missing files"
        );
        let mut records = Vec::new();
        let mut total = 0u64;
        for path in paths {
            let meta = root.metadata(Path::new(&path))?;
            ensure!(meta.is_file(), "package object payload is not regular");
            let bytes = root.read(
                Path::new(&path),
                if path == MANIFEST_FILE {
                    MAX_MANIFEST_BYTES
                } else {
                    self.limits.max_file_bytes
                },
            )?;
            total = total
                .checked_add(bytes.len() as u64)
                .context("package object size overflow")?;
            ensure!(
                total <= self.limits.max_package_bytes as u64,
                "package object byte budget exceeded"
            );
            records.push(FileRecord {
                executable: path != MANIFEST_FILE && meta.permissions().mode() & 0o111 != 0,
                path,
                sealed: SealedFile {
                    bytes: bytes.len() as u64,
                    sha256: sha256(&bytes),
                },
            });
        }
        ensure!(
            tree_digest(&records) == pin.sha256,
            "package content digest changed"
        );
        validate_resources(&root, &manifest)?;
        let seal = records
            .into_iter()
            .map(|record| (PathBuf::from(record.path), record.sealed))
            .collect();
        Ok(PackageObject {
            pin: pin.clone(),
            manifest,
            execution_allowed: false,
            root: root.sealed(seal),
        })
    }
}
fn list_files(
    base: &Path,
    path: &Path,
    depth: usize,
    entries: &mut usize,
    result: &mut BTreeSet<String>,
) -> Result<()> {
    ensure!(depth <= 128, "package directory depth exceeded");
    for entry in fs::read_dir(path)? {
        *entries += 1;
        ensure!(*entries <= 8192, "package object entries exceeded");
        let entry = entry?;
        let kind = entry.file_type()?;
        ensure!(!kind.is_symlink(), "package objects cannot contain links");
        if kind.is_dir() {
            list_files(base, &entry.path(), depth + 1, entries, result)?;
        } else {
            ensure!(
                kind.is_file(),
                "package objects cannot contain special files"
            );
            let relative = entry
                .path()
                .strip_prefix(base)?
                .to_str()
                .context("package object path is not UTF-8")?
                .to_owned();
            super::manifest::relative(&relative)?;
            result.insert(relative);
        }
    }
    Ok(())
}

pub(crate) fn validate_resources(root: &Root, manifest: &PackageManifest) -> Result<()> {
    use crate::resources::frontmatter;
    for path in &manifest.resources.instructions {
        let bytes = root.read(Path::new(path), 64 * 1024)?;
        std::str::from_utf8(&bytes).context("package instructions are not UTF-8")?;
    }
    for path in &manifest.resources.skills {
        let path = Path::new(path);
        ensure!(
            path.file_name().is_some_and(|p| p == "SKILL.md"),
            "skill entry must name SKILL.md"
        );
        let bytes = root.read(path, 64 * 1024)?;
        let parsed = frontmatter::parse(std::str::from_utf8(&bytes)?, true)?;
        let name = parsed.fields.get("name").context("skill name missing")?;
        ensure!(
            frontmatter::valid_name(name)
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|p| p == name.as_str()),
            "skill name/directory mismatch"
        );
        let description = parsed
            .fields
            .get("description")
            .context("skill description missing")?;
        ensure!(
            !description.trim().is_empty() && description.chars().count() <= 1024,
            "invalid skill description"
        );
    }
    for path in &manifest.resources.prompts {
        let path = Path::new(path);
        let name = path
            .file_stem()
            .and_then(|p| p.to_str())
            .context("prompt name missing")?;
        ensure!(
            frontmatter::valid_name(name)
                && !matches!(name, "exit" | "reload" | "resources" | "config"),
            "invalid package prompt name"
        );
        let bytes = root.read(path, 64 * 1024)?;
        frontmatter::parse(std::str::from_utf8(&bytes)?, false)?;
    }
    for path in &manifest.resources.themes {
        let bytes = root.read(Path::new(path), 4096)?;
        let palette: serde_json::Value = serde_json::from_slice(&bytes)?;
        ensure!(palette.is_object(), "package theme must be a JSON object");
    }
    for executable in &manifest.executables {
        if let PackageProgram::Bundled { path } = &executable.program {
            ensure!(
                root.metadata(Path::new(path))?.permissions().mode() & 0o111 != 0,
                "bundled program is not executable"
            );
        }
    }
    Ok(())
}
