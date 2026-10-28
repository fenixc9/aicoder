use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};

use super::ToolFailure;

#[derive(Debug, Clone)]
pub struct ToolContext {
    workspace_root: PathBuf,
    tool_timeout: Duration,
    max_output_bytes: usize,
}

impl ToolContext {
    pub fn new(
        workspace_root: impl AsRef<Path>,
        tool_timeout: Duration,
        max_output_bytes: usize,
    ) -> Result<Self> {
        let workspace_root = std::fs::canonicalize(workspace_root.as_ref())
            .with_context(|| format!("Invalid workspace: {}", workspace_root.as_ref().display()))?;
        if !workspace_root.is_dir() {
            anyhow::bail!("Workspace is not a directory: {}", workspace_root.display());
        }

        Ok(Self {
            workspace_root,
            tool_timeout,
            max_output_bytes,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub(crate) fn tool_timeout(&self) -> Duration {
        self.tool_timeout
    }

    pub(crate) fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    pub(crate) fn resolve_existing(&self, path: &str) -> Result<PathBuf, ToolFailure> {
        let candidate = self.candidate(path);
        let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
            ToolFailure::new(
                "path_not_found",
                format!("Cannot resolve {}: {error}", candidate.display()),
            )
        })?;
        self.ensure_inside_workspace(resolved)
    }

    pub(crate) fn resolve_for_write(&self, path: &str) -> Result<PathBuf, ToolFailure> {
        let candidate = self.candidate(path);
        if candidate.exists() {
            return self.resolve_existing(path);
        }

        let parent = candidate
            .parent()
            .ok_or_else(|| ToolFailure::new("invalid_path", format!("Invalid path: {path}")))?;
        let resolved_parent = std::fs::canonicalize(parent).map_err(|error| {
            ToolFailure::new(
                "parent_not_found",
                format!(
                    "Parent directory does not exist: {} ({error})",
                    parent.display()
                ),
            )
        })?;
        self.ensure_inside_workspace(resolved_parent)?;
        Ok(candidate)
    }

    fn candidate(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        }
    }

    fn ensure_inside_workspace(&self, path: PathBuf) -> Result<PathBuf, ToolFailure> {
        if path.starts_with(&self.workspace_root) {
            Ok(path)
        } else {
            Err(ToolFailure::new(
                "path_outside_workspace",
                format!("Path is outside workspace: {}", path.display()),
            ))
        }
    }

    pub(crate) fn relative_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.workspace_root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}
