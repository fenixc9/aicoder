use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::WorkspaceFixture;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotEntry {
    File(Vec<u8>),
    Symlink(PathBuf),
}

/// Content snapshot of evaluation-relevant files, relative to a workspace root.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceSnapshot {
    entries: BTreeMap<PathBuf, SnapshotEntry>,
}

impl WorkspaceSnapshot {
    pub fn capture(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let mut entries = BTreeMap::new();
        capture_directory(root, root, &mut entries)?;
        Ok(Self { entries })
    }

    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.entries.keys().map(PathBuf::as_path)
    }

    pub fn contains(&self, relative_path: impl AsRef<Path>) -> bool {
        self.entries.contains_key(relative_path.as_ref())
    }

    pub fn file_content(&self, relative_path: impl AsRef<Path>) -> Option<&[u8]> {
        match self.entries.get(relative_path.as_ref()) {
            Some(SnapshotEntry::File(content)) => Some(content),
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

pub(crate) fn prepare_fixture(fixture: &WorkspaceFixture, destination: &Path) -> Result<()> {
    match fixture {
        WorkspaceFixture::Empty => Ok(()),
        WorkspaceFixture::CopyFrom(source) => {
            let source = source
                .canonicalize()
                .with_context(|| format!("Failed to resolve fixture {}", source.display()))?;
            copy_directory(&source, destination)
        }
        WorkspaceFixture::GitCheckout {
            repository,
            base_commit,
        } => prepare_git_checkout(repository, base_commit, destination),
    }
}

pub(crate) fn git_patch(workspace: &Path) -> Result<Option<String>> {
    if !workspace.join(".git").exists() {
        return Ok(None);
    }
    let index_directory = tempfile::tempdir().context("Failed to create temporary Git index")?;
    let index_path = index_directory.path().join("index");
    run_git_with_index(workspace, &index_path, ["read-tree", "HEAD"])?;
    run_git_with_index(workspace, &index_path, ["add", "-A", "--", "."])?;
    let output = run_git_with_index(
        workspace,
        &index_path,
        ["diff", "--cached", "--binary", "--no-ext-diff"],
    )?;
    String::from_utf8(output.stdout)
        .context("Generated Git patch is not valid UTF-8")
        .map(Some)
}

fn prepare_git_checkout(repository: &str, base_commit: &str, destination: &Path) -> Result<()> {
    ensure_safe_commit(base_commit)?;
    let output = Command::new("git")
        .args(["clone", "--quiet", "--no-checkout", "--"])
        .arg(repository)
        .arg(destination)
        .output()
        .context("Failed to start git clone for evaluation fixture")?;
    ensure_command_succeeded("git clone", output)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(destination)
        .args(["checkout", "--quiet", "--detach", "--force"])
        .arg(base_commit)
        .output()
        .context("Failed to start git checkout for evaluation fixture")?;
    ensure_command_succeeded("git checkout", output)?;
    Ok(())
}

fn run_git_with_index<const N: usize>(
    workspace: &Path,
    index_path: &Path,
    arguments: [&str; N],
) -> Result<Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(arguments)
        .env("GIT_INDEX_FILE", index_path)
        .output()
        .context("Failed to start git while extracting evaluation patch")?;
    ensure_command_succeeded("git patch extraction", output)
}

fn ensure_command_succeeded(operation: &str, output: Output) -> Result<Output> {
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("{operation} failed with {}: {stderr}", output.status)
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
) -> Result<()> {
    for item in fs::read_dir(directory)
        .with_context(|| format!("Failed to read workspace directory {}", directory.display()))?
    {
        let item = item.context("Failed to read workspace directory entry")?;
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
            capture_directory(root, &path, entries)?;
        } else if file_type.is_file() {
            entries.insert(
                relative.to_path_buf(),
                SnapshotEntry::File(
                    fs::read(&path)
                        .with_context(|| format!("Failed to snapshot {}", path.display()))?,
                ),
            );
        } else if file_type.is_symlink() {
            entries.insert(
                relative.to_path_buf(),
                SnapshotEntry::Symlink(
                    fs::read_link(&path)
                        .with_context(|| format!("Failed to read symlink {}", path.display()))?,
                ),
            );
        }
    }
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
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
        let source_path = item.path();
        let destination_path = destination.join(item.file_name());
        let file_type = item
            .file_type()
            .with_context(|| format!("Failed to inspect {}", source_path.display()))?;
        if file_type.is_dir() {
            if item.file_name() == "target" {
                continue;
            }
            copy_directory(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "Failed to copy fixture file {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
        } else if file_type.is_symlink() {
            copy_symlink(&source_path, &destination_path)?;
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
