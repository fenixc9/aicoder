use aicoder_core::tools::{ToolContext, ToolFailure};
use std::path::{Path, PathBuf};
/// Canonical workspace checks shared by the default IO tools. Not a process sandbox.
pub struct WorkspacePaths<'a> {
    workspace_root: &'a Path,
}
impl<'a> WorkspacePaths<'a> {
    pub fn from_context(context: &'a ToolContext) -> Self {
        Self {
            workspace_root: context.workspace_root(),
        }
    }
    pub fn resolve_existing(&self, path: &str) -> Result<PathBuf, ToolFailure> {
        let candidate = self.candidate(path);
        let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
            ToolFailure::new(
                "path_not_found",
                format!("Cannot resolve {}: {error}", candidate.display()),
            )
        })?;
        self.ensure_inside_workspace(resolved)
    }

    pub fn resolve_for_write(&self, path: &str) -> Result<PathBuf, ToolFailure> {
        let candidate = self.candidate(path);
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => return self.resolve_existing(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ToolFailure::new("invalid_path", error.to_string())),
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
        let resolved_parent = self.ensure_inside_workspace(resolved_parent)?;
        let name = candidate
            .file_name()
            .ok_or_else(|| ToolFailure::new("invalid_path", "File has no name"))?;
        Ok(resolved_parent.join(name))
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
        if path.starts_with(self.workspace_root) {
            Ok(path)
        } else {
            Err(ToolFailure::new(
                "path_outside_workspace",
                format!("Path is outside workspace: {}", path.display()),
            ))
        }
    }

    pub fn relative_path(&self, path: &Path) -> String {
        path.strip_prefix(self.workspace_root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}
