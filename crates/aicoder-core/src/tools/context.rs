use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};

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

    pub fn tool_timeout(&self) -> Duration {
        self.tool_timeout
    }

    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }
}
