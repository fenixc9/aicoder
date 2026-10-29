use crate::Setting;
use serde::{Deserialize, Serialize};
/// Limits apply to the complete discovery operation; overflow is an error, never partial context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourcePolicy {
    pub enabled: bool,
    pub global: bool,
    pub parents: bool,
    pub nested: bool,
    pub instructions: bool,
    pub system: bool,
    pub skills: bool,
    pub prompts: bool,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_scan_bytes: usize,
    pub max_files: usize,
    pub max_entries: usize,
    pub max_depth: usize,
    pub excluded_directories: Vec<String>,
}
impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            global: true,
            parents: true,
            nested: true,
            instructions: true,
            system: true,
            skills: true,
            prompts: true,
            max_file_bytes: 64 * 1024,
            max_total_bytes: 256 * 1024,
            max_scan_bytes: 4 * 1024 * 1024,
            max_files: 256,
            max_entries: 20000,
            max_depth: 32,
            excluded_directories: [
                ".git",
                ".hg",
                ".svn",
                "target",
                "node_modules",
                ".venv",
                ".aicoder",
                "artifacts",
                "dist",
                "build",
                "__pycache__",
                ".cache",
                ".tox",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        }
    }
}
impl ResourcePolicy {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.max_file_bytes > 0
                && self.max_total_bytes > 0
                && self.max_scan_bytes > 0
                && self.max_files > 0
                && self.max_entries > 0
                && self.max_depth > 0,
            "resource limits must be positive"
        );
        anyhow::ensure!(
            self.max_file_bytes <= 1024 * 1024
                && self.max_total_bytes <= 8 * 1024 * 1024
                && self.max_scan_bytes <= 32 * 1024 * 1024
                && self.max_files <= 1024
                && self.max_entries <= 100000
                && self.max_depth <= 64,
            "resource limits exceed supported discovery ceilings"
        );
        anyhow::ensure!(
            self.excluded_directories.len() <= 128,
            "too many excluded resource directories"
        );
        anyhow::ensure!(
            self.excluded_directories
                .iter()
                .all(|value| !value.is_empty()
                    && !value.contains(['/', '\\'])
                    && value != "."
                    && value != ".."),
            "excluded resource directories must be single path components"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceSettings {
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub enabled: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub global: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub parents: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub nested: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub instructions: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub system: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub skills: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub prompts: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_file_bytes: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_total_bytes: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_scan_bytes: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_files: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_entries: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_depth: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub excluded_directories: Setting<Vec<String>>,
}
