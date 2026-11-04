use aicoder_core::{events::EventDeliveryConfig, model::ResponseFormat};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::BTreeMap, path::PathBuf};

/// Missing inherits the lower layer; JSON null clears a nullable setting.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Setting<T> {
    #[default]
    Inherit,
    Clear,
    Value(T),
}
impl<T> Setting<T> {
    pub fn is_inherit(&self) -> bool {
        matches!(self, Self::Inherit)
    }
    pub fn from_option(value: Option<T>) -> Self {
        value.map_or(Self::Inherit, Self::Value)
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Setting<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Option::<T>::deserialize(d)?.map_or(Self::Clear, Self::Value))
    }
}
impl<T: Serialize> Serialize for Setting<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(value) => value.serialize(s),
            _ => s.serialize_none(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSettings {
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub reasoning_effort: Setting<aicoder_core::model::ReasoningEffort>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_output_tokens: Setting<u32>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub temperature: Setting<f32>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub top_p: Setting<f32>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub seed: Setting<i64>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub stop: Setting<Vec<String>>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub response_format: Setting<ResponseFormat>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionSettings {
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_auxiliary_calls: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub tool_timeout_ms: Setting<u64>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_rounds: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub stream: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_tool_calls: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_tokens: Setting<u64>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_duration_ms: Setting<u64>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub max_output_bytes: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub events: Setting<EventDeliveryConfig>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextStrategy {
    Prune,
    Semantic,
}
impl std::str::FromStr for ContextStrategy {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "prune" => Ok(Self::Prune),
            "semantic" => Ok(Self::Semantic),
            _ => anyhow::bail!("context strategy must be prune or semantic"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenizerMode {
    Heuristic,
    Auto,
    Cl100kBase,
    O200kBase,
}
impl std::str::FromStr for TokenizerMode {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "heuristic" => Ok(Self::Heuristic),
            "auto" => Ok(Self::Auto),
            "cl100k_base" => Ok(Self::Cl100kBase),
            "o200k_base" => Ok(Self::O200kBase),
            _ => anyhow::bail!("tokenizer must be heuristic, auto, cl100k_base or o200k_base"),
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextSettings {
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub tokenizer: Setting<TokenizerMode>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub strategy: Setting<ContextStrategy>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub summary_output_tokens: Setting<u32>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub window_tokens: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub reserved_output_tokens: Setting<usize>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub preserve_recent_tokens: Setting<usize>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub provider_profile: Setting<String>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub model: Setting<String>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub system_prompt: Setting<String>,
    pub options: ModelSettings,
    pub execution: ExecutionSettings,
    pub context: ContextSettings,
    pub resources: crate::resources::ResourceSettings,
    pub packages: crate::packages::PackageSettings,
    pub code_mode: crate::code_mode::CodeModeSettings,
}
/// Trusted routing descriptor. API keys belong to a credential source, never this file.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    #[serde(default)]
    pub classifiers: Vec<aicoder_core::auxiliary::ClassifierModel>,
    #[serde(default)]
    pub image_models: Vec<aicoder_core::auxiliary::ImageGenerationModel>,
    #[serde(default)]
    pub classifier_image_input: aicoder_providers::ClassifierImageInput,
    #[serde(default)]
    pub models: Vec<aicoder_core::model::ModelSpec>,
    pub base_url: String,
    pub api_key_env: String,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub compatibility: Option<aicoder_providers::CompatibilityProfile>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub connect_timeout: Option<u64>,
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub max_response_bytes: Option<usize>,
    #[serde(default)]
    pub stream_usage: Option<bool>,
}
impl std::fmt::Debug for ProviderProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderProfile")
            .field("routing", &"[validated on resolution]")
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettingsFile {
    pub version: u32,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProviderProfile>,
    #[serde(default)]
    pub session_dir: Setting<PathBuf>,
}
#[derive(Debug, Clone, Default)]
pub struct RuntimeOverrides {
    pub settings: Settings,
    /// Trusted explicit provider descriptor, above environment/file profile fields.
    pub provider: Option<ProviderProfile>,
    pub session_dir: Setting<PathBuf>,
}
#[derive(Debug, Clone)]
pub struct ConfigDiscovery {
    pub enabled: bool,
    pub user_file: Option<PathBuf>,
    pub project_file: Option<PathBuf>,
}
impl Default for ConfigDiscovery {
    fn default() -> Self {
        Self {
            enabled: true,
            user_file: None,
            project_file: None,
        }
    }
}

/// Frontend option parser: `default` clears, while `none` is an explicit model capability.
#[derive(Debug, Clone, Copy)]
pub struct ReasoningOverride(pub Option<aicoder_core::model::ReasoningEffort>);
impl std::str::FromStr for ReasoningOverride {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "default" {
            Ok(Self(None))
        } else {
            s.parse().map(|effort| Self(Some(effort)))
        }
    }
}
impl ReasoningOverride {
    pub fn setting(self) -> Setting<aicoder_core::model::ReasoningEffort> {
        self.0.map_or(Setting::Clear, Setting::Value)
    }
}
