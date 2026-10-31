use super::*;
use crate::ConfigSource;
use std::collections::HashSet;

pub(super) struct Loader<'a> {
    pub(super) packages: Arc<crate::packages::PackageSnapshot>,
    pub(super) package_pins: Vec<crate::packages::PackagePin>,
    pub(super) workspace_enabled: bool,
    pub(super) include_global: bool,
    config: &'a RuntimeConfig,
    user_root: Option<PathBuf>,
    policy: ResourcePolicy,
    pub(super) documents: Vec<Document>,
    pub(super) seen: HashSet<(ResourceKind, PathBuf)>,
    pub(super) skills: BTreeMap<String, Skill>,
    pub(super) prompts: BTreeMap<String, usize>,
    pub(super) entries: usize,
    pub(super) scanned: usize,
    pub(super) retained: usize,
    scopes: Vec<(Root, ResourceScope)>,
}
impl<'a> Loader<'a> {
    pub(super) fn new(config: &'a RuntimeConfig) -> Self {
        Self {
            packages: Arc::default(),
            package_pins: Vec::new(),
            workspace_enabled: true,
            include_global: true,
            config,
            user_root: config
                .state_root
                .as_ref()
                .and_then(|root| root.canonicalize().ok()),
            policy: config.resources.clone(),
            documents: Vec::new(),
            seen: HashSet::new(),
            skills: BTreeMap::new(),
            prompts: BTreeMap::new(),
            entries: 0,
            scanned: 0,
            retained: 0,
            scopes: Vec::new(),
        }
    }
    pub(super) fn load(mut self) -> Result<ResourceSnapshot> {
        self.policy.validate()?;
        if self.policy.enabled {
            if self.include_global
                && self.policy.global
                && let Some(root) = &self.config.state_root
                && root.is_dir()
            {
                self.scopes.push((Root::open(root)?, ResourceScope::Global));
            }
            if self.workspace_enabled {
                let mut ancestors = if self.policy.parents {
                    self.config
                        .workspace
                        .ancestors()
                        .map(Path::to_path_buf)
                        .collect::<Vec<_>>()
                } else {
                    vec![self.config.workspace.clone()]
                };
                ancestors.reverse();
                ensure!(
                    ancestors.len() <= self.policy.max_depth.saturating_add(1),
                    "resource ancestry exceeds depth limit"
                );
                for directory in ancestors {
                    let scope = if directory == self.config.workspace {
                        ResourceScope::Workspace
                    } else {
                        ResourceScope::Parent
                    };
                    if scope == ResourceScope::Parent && !self.policy.parents {
                        continue;
                    }
                    self.scopes.push((Root::open(&directory)?, scope));
                }
            }
            let mut packages_loaded = false;
            for (root, scope) in self.scopes.clone() {
                if scope != ResourceScope::Global && !packages_loaded {
                    self.load_packages()?;
                    packages_loaded = true;
                }
                if self.policy.instructions {
                    self.document(
                        &root,
                        Path::new("AGENTS.md"),
                        ResourceKind::Agents,
                        scope,
                        &root.path,
                    )?;
                }
                if self.policy.system {
                    for name in ["SYSTEM.md", "APPEND_SYSTEM.md"] {
                        let kind = if name == "SYSTEM.md" {
                            ResourceKind::System
                        } else {
                            ResourceKind::AppendSystem
                        };
                        self.document(&root, Path::new(name), kind, scope, &root.path)?;
                        if scope != ResourceScope::Global {
                            self.document(
                                &root,
                                &Path::new(".aicoder").join(name),
                                kind,
                                scope,
                                &root.path,
                            )?;
                        }
                    }
                }
                let prefix = if scope == ResourceScope::Global {
                    PathBuf::new()
                } else {
                    PathBuf::from(".aicoder")
                };
                if self.policy.skills {
                    self.collections(&root, &prefix.join("skills"), scope, true, 0)?;
                }
                if self.policy.prompts {
                    self.collections(&root, &prefix.join("prompts"), scope, false, 0)?;
                }
            }
            if !packages_loaded {
                self.load_packages()?;
            }
            if self.workspace_enabled && self.policy.nested && self.policy.instructions {
                let root = Root::open(&self.config.workspace)?;
                self.nested(&root, Path::new(""), 0)?;
            }
        }
        let system = self
            .documents
            .iter()
            .rposition(|doc| doc.info.kind == ResourceKind::System && doc.info.active);
        for (index, doc) in self
            .documents
            .iter_mut()
            .enumerate()
            .filter(|(_, doc)| doc.info.kind == ResourceKind::System)
        {
            doc.info.active = Some(index) == system;
        }
        let mut base_prompt = self.config.system_prompt.clone();
        if let Some(index) = system {
            let doc = &self.documents[index];
            let configured = self.config.sources.get("system_prompt");
            let rank = match configured {
                Some(ConfigSource::Explicit) => 4,
                Some(ConfigSource::Environment { .. } | ConfigSource::EnvironmentFile { .. }) => 3,
                Some(ConfigSource::Project { .. }) => 2,
                Some(ConfigSource::User { .. }) => 1,
                _ => 0,
            };
            let file_rank = if doc.info.scope == ResourceScope::Global {
                1
            } else {
                2
            };
            if file_rank > rank {
                base_prompt = Some(doc.text.as_deref().unwrap_or_default().to_string());
            } else {
                self.documents[index].info.active = false;
            }
        }
        let files = self
            .documents
            .iter()
            .map(|doc| doc.info.clone())
            .collect::<Vec<_>>();
        let generation = if self.package_pins.is_empty() {
            fingerprint(&serde_json::to_vec(&files)?)
        } else {
            fingerprint(&serde_json::to_vec(&(&files, &self.package_pins))?)
        };
        let manifest = ResourceManifest {
            mode: if self.workspace_enabled {
                "workspace".into()
            } else {
                "global".into()
            },
            version: 1,
            generation,
            workspace: self.config.workspace.clone(),
            policy: self.policy,
            files,
            packages: self.package_pins,
            scanned_entries: self.entries,
            scanned_bytes: self.scanned,
            retained_bytes: self.retained,
        };
        let system_replaced = self
            .documents
            .iter()
            .any(|doc| doc.info.kind == ResourceKind::System && doc.info.active);
        Ok(ResourceSnapshot {
            manifest,
            documents: self.documents,
            skills: self.skills,
            prompts: self.prompts,
            base_prompt,
            configured_prompt: self.config.system_prompt.clone(),
            system_replaced,
        })
    }
    fn load_packages(&mut self) -> Result<()> {
        let packages = self.packages.clone();
        for package in packages.packages() {
            self.package_pins.push(package.pin.clone());
            let resources = &package.manifest.resources;
            for (enabled, paths, kind) in [
                (
                    self.policy.instructions,
                    &resources.instructions,
                    ResourceKind::AppendSystem,
                ),
                (self.policy.skills, &resources.skills, ResourceKind::Skill),
                (
                    self.policy.prompts,
                    &resources.prompts,
                    ResourceKind::Prompt,
                ),
            ] {
                if enabled {
                    for path in paths {
                        self.entries += 1;
                        ensure!(
                            self.entries <= self.policy.max_entries,
                            "package resource entry limit exceeded"
                        );
                        let workspace = self.config.workspace.clone();
                        ensure!(
                            self.document(
                                &package.root,
                                Path::new(path),
                                kind,
                                ResourceScope::Package,
                                &workspace
                            )?
                            .is_some(),
                            "declared package resource is missing or duplicated"
                        );
                    }
                }
            }
        }
        Ok(())
    }
    fn document(
        &mut self,
        root: &Root,
        relative: &Path,
        kind: ResourceKind,
        scope: ResourceScope,
        directory: &Path,
    ) -> Result<Option<usize>> {
        let path = root.path.join(relative);
        if scope != ResourceScope::Global && self.is_user_root(path.parent()) {
            return Ok(None);
        }
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("Cannot inspect resource"),
        };
        ensure!(
            !meta.file_type().is_symlink(),
            "resource files cannot be symbolic links: {}",
            path.display()
        );
        ensure!(
            meta.is_file(),
            "resource is not a regular file: {}",
            path.display()
        );
        let canonical = path.canonicalize()?;
        if !self.seen.insert((kind, canonical.clone())) {
            return Ok(None);
        }
        ensure!(
            self.documents.len() < self.policy.max_files,
            "resource file count exceeds limit"
        );
        let bytes = root
            .read(relative, self.policy.max_file_bytes)
            .with_context(|| format!("Cannot read resource {}", path.display()))?;
        self.scanned = self
            .scanned
            .checked_add(bytes.len())
            .context("resource byte count overflow")?;
        ensure!(
            self.scanned <= self.policy.max_scan_bytes,
            "resource scan byte limit exceeded"
        );
        let text = std::str::from_utf8(&bytes)
            .with_context(|| format!("Resource is not UTF-8: {}", path.display()))?;
        let (retained, stored) = if kind == ResourceKind::Skill {
            (0, None)
        } else {
            (bytes.len(), Some(Arc::<str>::from(text)))
        };
        self.retain(retained)?;
        let index = self.documents.len();
        self.documents.push(Document {
            info: ResourceInfo {
                kind,
                path: canonical,
                scope,
                directory: directory.to_path_buf(),
                name: None,
                description: None,
                bytes: bytes.len(),
                fingerprint: fingerprint(&bytes),
                active: true,
                manual_only: false,
            },
            text: stored,
            root: root.clone(),
            relative: relative.to_path_buf(),
        });
        if kind == ResourceKind::Skill {
            let parsed = frontmatter::parse(text, true)
                .with_context(|| format!("Invalid skill metadata in {}", path.display()))?;
            let name = parsed
                .fields
                .get("name")
                .context("Skill name is required")?
                .clone();
            ensure!(
                frontmatter::valid_name(&name),
                "Invalid skill name in {}",
                path.display()
            );
            let directory = relative.parent().context("Skill package is missing")?;
            ensure!(
                directory
                    .file_name()
                    .is_some_and(|part| part == name.as_str()),
                "Skill name must match its package directory"
            );
            let description = parsed
                .fields
                .get("description")
                .context("Skill description is required")?
                .trim()
                .to_string();
            ensure!(
                !description.is_empty() && description.chars().count() <= 1024,
                "Skill description must contain 1 to 1024 characters"
            );
            let manual = parsed
                .fields
                .get("disable-model-invocation")
                .is_some_and(|value| value == "true");
            self.retain(name.len() + description.len())?;
            let package = root.directory(directory)?;
            if let Some(previous) = self.skills.insert(name.clone(), Skill { index, package }) {
                ensure!(
                    self.documents[previous.index].info.scope != scope
                        || (scope != ResourceScope::Package
                            && self.documents[previous.index].root.path != root.path),
                    "Duplicate skill name in the same discovery root"
                );
                self.documents[previous.index].info.active = false;
            }
            self.documents[index].info.name = Some(name);
            self.documents[index].info.description = Some(description);
            self.documents[index].info.manual_only = manual;
        } else if kind == ResourceKind::Prompt {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .context("Prompt name is not Unicode")?
                .to_string();
            ensure!(
                frontmatter::valid_name(&name)
                    && !matches!(name.as_str(), "exit" | "reload" | "resources" | "config"),
                "Invalid or reserved prompt template name"
            );
            let parsed = frontmatter::parse(text, false)?;
            let description = parsed
                .fields
                .get("description")
                .cloned()
                .unwrap_or_else(|| format!("Prompt template {name}"));
            ensure!(
                description.chars().count() <= 1024,
                "Prompt description is too long"
            );
            if let Some(previous) = self.prompts.insert(name.clone(), index) {
                ensure!(
                    self.documents[previous].info.scope != scope
                        || (scope != ResourceScope::Package
                            && self.documents[previous].root.path != root.path),
                    "Duplicate prompt name in the same discovery root"
                );
                self.documents[previous].info.active = false;
            }
            self.documents[index].info.name = Some(name);
            self.documents[index].info.description = Some(description);
        }
        Ok(Some(index))
    }
    fn retain(&mut self, bytes: usize) -> Result<()> {
        self.retained = self
            .retained
            .checked_add(bytes)
            .context("resource retained byte count overflow")?;
        ensure!(
            self.retained <= self.policy.max_total_bytes,
            "resource retained byte limit exceeded"
        );
        Ok(())
    }
    fn is_user_root(&self, path: Option<&Path>) -> bool {
        path.and_then(|path| path.canonicalize().ok())
            .as_ref()
            .zip(self.user_root.as_ref())
            .is_some_and(|(path, user)| path == user)
    }
    fn entries(&mut self, directory: &Path) -> Result<Vec<std::fs::DirEntry>> {
        let read = match std::fs::read_dir(directory) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("Cannot enumerate resource directory"),
        };
        let mut entries = Vec::new();
        for entry in read {
            self.entries += 1;
            ensure!(
                self.entries <= self.policy.max_entries,
                "resource discovery entry limit exceeded"
            );
            entries.push(entry?);
        }
        entries.sort_by_key(|entry| entry.file_name());
        Ok(entries)
    }
    fn collections(
        &mut self,
        root: &Root,
        relative: &Path,
        scope: ResourceScope,
        skills: bool,
        depth: usize,
    ) -> Result<()> {
        let path = root.path.join(relative);
        if scope != ResourceScope::Global && self.is_user_root(path.parent()) {
            return Ok(());
        }
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink() && meta.is_dir(),
                "resource collection must be a directory without links"
            ),
        };
        root.directory(relative)?;
        ensure!(
            depth <= self.policy.max_depth,
            "resource collection depth limit exceeded"
        );
        if skills && std::fs::symlink_metadata(path.join("SKILL.md")).is_ok() {
            self.document(
                root,
                &relative.join("SKILL.md"),
                ResourceKind::Skill,
                scope,
                &root.path,
            )?;
            return Ok(());
        }
        for entry in self.entries(&path)? {
            let meta = entry.file_type()?;
            let relative = relative.join(entry.file_name());
            if meta.is_symlink() {
                anyhow::bail!(
                    "resource collection links are not allowed: {}",
                    entry.path().display()
                );
            }
            if meta.is_dir() {
                if !self
                    .policy
                    .excluded_directories
                    .iter()
                    .any(|name| entry.file_name() == name.as_str())
                {
                    self.collections(root, &relative, scope, skills, depth + 1)?;
                }
            } else if skills && entry.file_name() == "SKILL.md" {
                self.document(root, &relative, ResourceKind::Skill, scope, &root.path)?;
            } else if !skills
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "md")
            {
                self.document(root, &relative, ResourceKind::Prompt, scope, &root.path)?;
            }
        }
        Ok(())
    }
    fn nested(&mut self, root: &Root, relative: &Path, depth: usize) -> Result<()> {
        ensure!(
            depth <= self.policy.max_depth,
            "nested instruction depth limit exceeded"
        );
        for entry in self.entries(&root.path.join(relative))? {
            let kind = entry.file_type()?;
            if !kind.is_dir() || kind.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            if self
                .policy
                .excluded_directories
                .iter()
                .any(|excluded| name == excluded.as_str())
            {
                continue;
            }
            let child = relative.join(name);
            let directory = root.path.join(&child);
            if self.is_user_root(Some(&directory)) {
                continue;
            }
            self.document(
                root,
                &child.join("AGENTS.md"),
                ResourceKind::Agents,
                ResourceScope::Nested,
                &directory,
            )?;
            self.nested(root, &child, depth + 1)?;
        }
        Ok(())
    }
}
