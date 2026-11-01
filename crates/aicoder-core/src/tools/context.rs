use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};

#[derive(Clone)]
pub struct ToolContext {
    workspace_root: PathBuf,
    tool_timeout: Duration,
    max_output_bytes: usize,
    ui: crate::ui::UiClient,
    artifact_store: Option<Arc<dyn crate::artifacts::ArtifactStore>>,
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
            ui: crate::ui::UiClient::default(),
            artifact_store: None,
        })
    }

    pub fn ui(&self) -> &crate::ui::UiClient {
        &self.ui
    }
    pub fn with_ui(mut self, ui: crate::ui::UiClient) -> Self {
        self.ui = ui;
        self
    }
    pub(crate) fn bounded_output(mut self, max: usize) -> Self {
        self.max_output_bytes = self.max_output_bytes.min(max);
        self
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

impl ToolContext {
    pub fn with_artifact_store(mut self, store: Arc<dyn crate::artifacts::ArtifactStore>) -> Self {
        self.artifact_store = Some(store);
        self
    }
    pub fn artifact_store(&self) -> Option<&Arc<dyn crate::artifacts::ArtifactStore>> {
        self.artifact_store.as_ref()
    }
}
impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("workspace_root", &self.workspace_root)
            .field("tool_timeout", &self.tool_timeout)
            .field("max_output_bytes", &self.max_output_bytes)
            .field("artifacts_enabled", &self.artifact_store.is_some())
            .finish()
    }
}
