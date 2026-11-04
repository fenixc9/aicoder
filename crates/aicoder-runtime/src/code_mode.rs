//! Optional engine assembly; settings select capabilities, never JS host IO or credentials.
use crate::Setting;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodeModeSettings {
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub model_profiles: Setting<Vec<String>>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub only: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub enabled: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub tools: Setting<Vec<String>>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct CodeModePolicy {
    pub model_profiles: Vec<String>,
    pub only: bool,
    pub enabled: bool,
    pub tools: Option<Vec<String>>,
}
impl CodeModePolicy {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        let mut profiles = std::collections::BTreeSet::new();
        anyhow::ensure!(
            self.model_profiles.len() <= 16
                && self.model_profiles.iter().all(|name| !name.is_empty()
                    && name.len() <= 128
                    && !name.chars().any(char::is_control)
                    && profiles.insert(name)),
            "invalid auxiliary profile selection"
        );
        anyhow::ensure!(
            self.model_profiles.is_empty() || cfg!(feature = "auxiliary-models"),
            "auxiliary profiles require the auxiliary-models build feature"
        );
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
#[cfg(feature = "auxiliary-models")]
#[derive(Debug, Clone)]
pub(crate) struct ClassifierRoute {
    pub config: aicoder_providers::ClientConfig,
    pub models: Vec<aicoder_core::auxiliary::ClassifierModel>,
    pub image_models: Vec<aicoder_core::auxiliary::ImageGenerationModel>,
    pub images: aicoder_providers::ClassifierImageInput,
}
