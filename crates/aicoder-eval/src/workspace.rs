use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::WorkspaceFixture;

/// Hard admission limits for filesystem capture/copy; exhaustion is an error, never a partial diff.
#[derive(Debug, Clone, Copy)]
pub struct WorkspaceLimits {
    pub max_entries: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_depth: usize,
}
impl Default for WorkspaceLimits {
    fn default() -> Self {
        Self {
            max_entries: 20_000,
            max_file_bytes: 16 * 1024 * 1024,
            max_total_bytes: 128 * 1024 * 1024,
            max_depth: 64,
        }
    }
}
struct Budget {
    limits: WorkspaceLimits,
    entries: usize,
    bytes: usize,
}
impl Budget {
    fn new(limits: WorkspaceLimits) -> Result<Self> {
        ensure!(
            (1..=100_000).contains(&limits.max_entries)
                && (1..=64 * 1024 * 1024).contains(&limits.max_file_bytes)
                && (1..=512 * 1024 * 1024).contains(&limits.max_total_bytes)
                && (1..=128).contains(&limits.max_depth),
            "invalid workspace capture limits"
        );
        Ok(Self {
            limits,
            entries: 0,
            bytes: 0,
        })
    }
    fn entry(&mut self) -> Result<()> {
        self.entries += 1;
        ensure!(
            self.entries <= self.limits.max_entries,
            "workspace entry budget exhausted"
        );
        Ok(())
    }
    fn read(&mut self, path: &Path) -> Result<Vec<u8>> {
        let limit = self
            .limits
            .max_file_bytes
            .min(self.limits.max_total_bytes.saturating_sub(self.bytes));
        let bytes = crate::bounds::read(path, limit)?;
        self.bytes += bytes.len();
        Ok(bytes)
    }
}
#[cfg(unix)]
fn executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn executable(_: &fs::Metadata) -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotEntry {
    File { bytes: Vec<u8>, executable: bool },
    Symlink(PathBuf),
}

/// Content snapshot of evaluation-relevant files, relative to a workspace root.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceSnapshot {
    entries: BTreeMap<PathBuf, SnapshotEntry>,
}

impl WorkspaceSnapshot {
    pub fn capture(root: impl AsRef<Path>) -> Result<Self> {
        Self::capture_with_limits(root, WorkspaceLimits::default())
    }
    pub fn capture_with_limits(root: impl AsRef<Path>, limits: WorkspaceLimits) -> Result<Self> {
        let mut entries = BTreeMap::new();
        capture_directory(
            root.as_ref(),
            root.as_ref(),
            &mut entries,
            &mut Budget::new(limits)?,
            0,
        )?;
        Ok(Self { entries })
    }
    /// Exact content/type/executable-bit identity. Ignores .git/target as declared by capture.
    pub fn fingerprint(&self) -> String {
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        for (path, entry) in &self.entries {
            let path = path.as_os_str().as_encoded_bytes();
            digest.update(&(path.len() as u64).to_le_bytes());
            digest.update(path);
            match entry {
                SnapshotEntry::File { bytes, executable } => {
                    digest.update(&[0, u8::from(*executable)]);
                    digest.update(&(bytes.len() as u64).to_le_bytes());
                    digest.update(bytes);
                }
                SnapshotEntry::Symlink(target) => {
                    digest.update(&[1]);
                    let bytes = target.as_os_str().as_encoded_bytes();
                    digest.update(&(bytes.len() as u64).to_le_bytes());
                    digest.update(bytes);
                }
            }
        }
        digest
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Bounded applicable UTF-8 diff for non-Git fixtures. Binary/non-UTF-8 paths return None.
    pub fn unified_patch(&self, after: &Self) -> Result<Option<String>> {
        use std::io::Write;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        let mut writer = crate::bounds::LimitedWriter {
            inner: Vec::new(),
            remaining: 8 * 1024 * 1024,
        };
        let diff = WorkspaceDiff::between(self, after);
        for name in diff.changed_paths() {
            ensure!(
                std::time::Instant::now() < deadline,
                "workspace diff computation budget exhausted"
            );
            if name.chars().any(char::is_control) {
                return Ok(None);
            }
            let before = self.entries.get(Path::new(name));
            let next = after.entries.get(Path::new(name));
            fn content(entry: Option<&SnapshotEntry>) -> Option<(&str, &'static str)> {
                match entry {
                    None => Some(("", "000000")),
                    Some(SnapshotEntry::File { bytes, executable }) => Some((
                        std::str::from_utf8(bytes).ok()?,
                        if *executable { "100755" } else { "100644" },
                    )),
                    Some(SnapshotEntry::Symlink(target)) => Some((target.to_str()?, "120000")),
                }
            }
            let Some((old, old_mode)) = content(before) else {
                return Ok(None);
            };
            let Some((new, new_mode)) = content(next) else {
                return Ok(None);
            };
            let a = serde_json::to_string(&format!("a/{name}"))?;
            let b = serde_json::to_string(&format!("b/{name}"))?;
            writeln!(writer, "diff --git {a} {b}")?;
            if before.is_none() {
                writeln!(writer, "new file mode {new_mode}")?;
            } else if next.is_none() {
                writeln!(writer, "deleted file mode {old_mode}")?;
            } else if old_mode != new_mode {
                writeln!(writer, "old mode {old_mode}\nnew mode {new_mode}")?;
            }
            let diff = similar::TextDiff::configure()
                .deadline(deadline)
                .diff_lines(old, new);
            diff.unified_diff()
                .header(
                    if before.is_none() { "/dev/null" } else { &a },
                    if next.is_none() { "/dev/null" } else { &b },
                )
                .to_writer(&mut writer)?;
        }
        Ok(Some(String::from_utf8(writer.inner)?))
    }

    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.entries.keys().map(PathBuf::as_path)
    }

    pub fn contains(&self, relative_path: impl AsRef<Path>) -> bool {
        self.entries.contains_key(relative_path.as_ref())
    }

    pub fn file_content(&self, relative_path: impl AsRef<Path>) -> Option<&[u8]> {
        match self.entries.get(relative_path.as_ref()) {
            Some(SnapshotEntry::File { bytes, .. }) => Some(bytes),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceDiff {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
}

impl WorkspaceDiff {
    pub fn between(before: &WorkspaceSnapshot, after: &WorkspaceSnapshot) -> Self {
        let before_paths = before.entries.keys().collect::<BTreeSet<_>>();
        let after_paths = after.entries.keys().collect::<BTreeSet<_>>();
        let added = after_paths
            .difference(&before_paths)
            .map(|path| display_relative(path))
            .collect();
        let deleted = before_paths
            .difference(&after_paths)
            .map(|path| display_relative(path))
            .collect();
        let modified = before_paths
            .intersection(&after_paths)
            .filter(|path| before.entries.get(**path) != after.entries.get(**path))
            .map(|path| display_relative(path))
            .collect();
        Self {
            added,
            modified,
            deleted,
        }
    }

    pub fn changed_paths(&self) -> impl Iterator<Item = &str> {
        self.added
            .iter()
            .chain(&self.modified)
            .chain(&self.deleted)
            .map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }
}

pub(crate) async fn prepare_fixture(
    fixture: &WorkspaceFixture,
    destination: &Path,
    context: aicoder_core::TurnExecutionContext,
    excluded: &[String],
) -> Result<()> {
    match fixture {
        WorkspaceFixture::Empty => Ok(()),
        WorkspaceFixture::CopyFrom(source) => {
            let source = source
                .canonicalize()
                .with_context(|| format!("Failed to resolve fixture {}", source.display()))?;
            copy_directory(
                &source,
                destination,
                &mut Budget::new(WorkspaceLimits::default())?,
                0,
            )
        }
        WorkspaceFixture::GitCheckout {
            repository,
            base_commit,
        } => prepare_git_checkout(repository, base_commit, destination, context, excluded).await,
    }
}

pub(crate) async fn git_patch(
    workspace: &Path,
    context: aicoder_core::TurnExecutionContext,
    excluded: &[String],
) -> Result<Option<String>> {
    if !workspace.join(".git").exists() {
        return Ok(None);
    }
    let directory = tempfile::tempdir()?;
    let index = directory.path().join("index");
    crate::git::run(
        workspace,
        ["read-tree", "HEAD"].into_iter().map(Into::into).collect(),
        Some(&index),
        context.clone(),
        excluded,
    )
    .await?;
    crate::git::run(
        workspace,
        ["add", "-A", "--", "."]
            .into_iter()
            .map(Into::into)
            .collect(),
        Some(&index),
        context.clone(),
        excluded,
    )
    .await?;
    let bytes = crate::git::run(
        workspace,
        [
            "diff",
            "--cached",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        Some(&index),
        context,
        excluded,
    )
    .await?;
    Ok(Some(String::from_utf8(bytes)?))
}
async fn prepare_git_checkout(
    repository: &str,
    base_commit: &str,
    destination: &Path,
    context: aicoder_core::TurnExecutionContext,
    excluded: &[String],
) -> Result<()> {
    ensure_safe_commit(base_commit)?;
    let repository = if Path::new(repository).exists() {
        Path::new(repository).canonicalize()?.into_os_string()
    } else {
        repository.into()
    };
    crate::git::run(
        destination,
        vec![
            "clone".into(),
            "--quiet".into(),
            "--no-checkout".into(),
            "--".into(),
            repository,
            destination.as_os_str().into(),
        ],
        None,
        context.clone(),
        excluded,
    )
    .await?;
    crate::git::run(
        destination,
        vec![
            "checkout".into(),
            "--quiet".into(),
            "--detach".into(),
            "--force".into(),
            base_commit.into(),
        ],
        None,
        context,
        excluded,
    )
    .await?;
    Ok(())
}

fn ensure_safe_commit(commit: &str) -> Result<()> {
    if (7..=64).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        anyhow::bail!("Invalid Git base commit: {commit}")
    }
}

fn capture_directory(
    root: &Path,
    directory: &Path,
    entries: &mut BTreeMap<PathBuf, SnapshotEntry>,
    budget: &mut Budget,
    depth: usize,
) -> Result<()> {
    ensure!(
        depth <= budget.limits.max_depth,
        "workspace snapshot depth budget exhausted"
    );
    for item in fs::read_dir(directory)
        .with_context(|| format!("Failed to read workspace directory {}", directory.display()))?
    {
        let item = item.context("Failed to read workspace directory entry")?;
        budget.entry()?;
        let path = item.path();
        let relative = path
            .strip_prefix(root)
            .context("Workspace entry escaped snapshot root")?;
        let file_type = item
            .file_type()
            .with_context(|| format!("Failed to inspect {}", path.display()))?;
        if file_type.is_dir() {
            if ignored_directory(relative) {
                continue;
            }
            capture_directory(root, &path, entries, budget, depth + 1)?;
        } else if file_type.is_file() {
            entries.insert(
                relative.to_path_buf(),
                SnapshotEntry::File {
                    bytes: budget.read(&path)?,
                    executable: executable(&fs::symlink_metadata(&path)?),
                },
            );
        } else if file_type.is_symlink() {
            entries.insert(
                relative.to_path_buf(),
                SnapshotEntry::Symlink(
                    fs::read_link(&path)
                        .with_context(|| format!("Failed to read symlink {}", path.display()))?,
                ),
            );
        } else {
            anyhow::bail!("special files are unsupported in workspace snapshots");
        }
    }
    Ok(())
}

fn copy_directory(
    source: &Path,
    destination: &Path,
    budget: &mut Budget,
    depth: usize,
) -> Result<()> {
    ensure!(
        depth <= budget.limits.max_depth,
        "fixture copy depth budget exhausted"
    );
    fs::create_dir_all(destination).with_context(|| {
        format!(
            "Failed to create evaluation workspace {}",
            destination.display()
        )
    })?;
    for item in fs::read_dir(source)
        .with_context(|| format!("Failed to read fixture directory {}", source.display()))?
    {
        let item = item.context("Failed to read fixture directory entry")?;
        budget.entry()?;
        let source_path = item.path();
        let destination_path = destination.join(item.file_name());
        let file_type = item
            .file_type()
            .with_context(|| format!("Failed to inspect {}", source_path.display()))?;
        if file_type.is_dir() {
            if item.file_name() == "target" {
                continue;
            }
            copy_directory(&source_path, &destination_path, budget, depth + 1)?;
        } else if file_type.is_file() {
            let bytes = budget.read(&source_path)?;
            fs::write(&destination_path, bytes)?;
            fs::set_permissions(&destination_path, fs::metadata(&source_path)?.permissions())?;
        } else if file_type.is_symlink() {
            copy_symlink(&source_path, &destination_path)?;
        } else {
            anyhow::bail!("special files are unsupported in fixture copies");
        }
    }
    Ok(())
}

fn ignored_directory(relative: &Path) -> bool {
    relative.components().any(|component| {
        let component = component.as_os_str();
        component == ".git" || component == "target"
    })
}

fn display_relative(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(unix)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    let target = fs::read_link(source)
        .with_context(|| format!("Failed to read symlink {}", source.display()))?;
    std::os::unix::fs::symlink(&target, destination).with_context(|| {
        format!(
            "Failed to copy symlink {} to {}",
            source.display(),
            destination.display()
        )
    })
}

#[cfg(windows)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    let target = fs::read_link(source)
        .with_context(|| format!("Failed to read symlink {}", source.display()))?;
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(&target, destination)
    } else {
        std::os::windows::fs::symlink_file(&target, destination)
    }
    .with_context(|| {
        format!(
            "Failed to copy symlink {} to {}",
            source.display(),
            destination.display()
        )
    })
}

#[cfg(not(any(unix, windows)))]
fn copy_symlink(source: &Path, _destination: &Path) -> Result<()> {
    anyhow::bail!("Cannot copy symlink {} on this platform", source.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_diff_tracks_content_changes() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("existing.txt"), "before").unwrap();
        fs::write(directory.path().join("deleted.txt"), "gone").unwrap();
        let before = WorkspaceSnapshot::capture(directory.path()).unwrap();

        fs::write(directory.path().join("existing.txt"), "after").unwrap();
        fs::write(directory.path().join("added.txt"), "new").unwrap();
        fs::remove_file(directory.path().join("deleted.txt")).unwrap();
        let after = WorkspaceSnapshot::capture(directory.path()).unwrap();

        assert_eq!(
            WorkspaceDiff::between(&before, &after),
            WorkspaceDiff {
                added: vec!["added.txt".to_string()],
                modified: vec!["existing.txt".to_string()],
                deleted: vec!["deleted.txt".to_string()],
            }
        );
    }
}

#[cfg(test)]
mod binary_patch_tests {
    use super::*;
    #[tokio::test]
    async fn git_binary_patch_keeps_all_raw_bytes_beyond_the_text_preview() {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(root.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet"]);
        let original = vec![0u8; 131072];
        std::fs::write(root.path().join("binary.dat"), &original).unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "base",
        ]);
        let mut state = 123456789u32;
        let changed = (0..131072)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect::<Vec<_>>();
        std::fs::write(root.path().join("binary.dat"), &changed).unwrap();
        let patch = git_patch(root.path(), aicoder_core::TurnExecutionContext::new(), &[])
            .await
            .unwrap()
            .unwrap();
        assert!(patch.len() > 65536 && patch.contains("GIT binary patch"));
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), patch).unwrap();
        git(&["apply", "--reverse", file.path().to_str().unwrap()]);
        assert_eq!(
            std::fs::read(root.path().join("binary.dat")).unwrap(),
            original
        );
        git(&["apply", file.path().to_str().unwrap()]);
        assert_eq!(
            std::fs::read(root.path().join("binary.dat")).unwrap(),
            changed
        );
    }
}
