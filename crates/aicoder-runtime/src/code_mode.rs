//! Optional engine assembly; settings select capabilities, never JS host IO or credentials.
use crate::Setting;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodeModeSettings {
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub only: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub enabled: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub tools: Setting<Vec<String>>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct CodeModePolicy {
    pub only: bool,
    pub enabled: bool,
    pub tools: Option<Vec<String>>,
}
impl CodeModePolicy {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.only || self.enabled,
            "Code Mode only requires Code Mode to be enabled"
        );
        if let Some(tools) = &self.tools {
            anyhow::ensure!(
                tools.len() <= 256,
                "Code Mode tool selection exceeds 256 entries"
            );
            let mut seen = std::collections::BTreeSet::new();
            for name in tools {
                anyhow::ensure!(
                    !name.is_empty()
                        && name.len() <= 128
                        && !name.chars().any(char::is_control)
                        && seen.insert(name),
                    "Invalid or duplicate Code Mode tool selection"
                );
            }
        }
        anyhow::ensure!(
            !self.enabled || cfg!(feature = "code-mode"),
            "Code Mode requires a runtime build with the code-mode feature"
        );
        Ok(())
    }
}
