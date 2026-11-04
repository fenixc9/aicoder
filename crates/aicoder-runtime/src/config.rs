use crate::{environment::*, settings::*};
use aicoder_core::{ContextWindowConfig, RunBudget, TurnExecutionConfig, model::ModelOptions};
use aicoder_providers::{ClientConfig, CompatibilityProfile, ProxyMode};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub const SETTINGS_VERSION: u32 = 1;
pub const MAX_SETTINGS_BYTES: u64 = 256 * 1024;
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a coding assistant. Inspect the project before making changes, use tools when needed, verify changes, and report concisely in the user's language.";
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigSource {
    Default,
    User { path: PathBuf },
    Project { path: PathBuf },
    Environment { variable: String },
    EnvironmentFile { path: PathBuf, variable: String },
    Explicit,
}
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeConfig {
    #[cfg(feature = "auxiliary-models")]
    #[serde(skip)]
    pub(crate) classifier_routes: Vec<crate::code_mode::ClassifierRoute>,
    pub version: u32,
    pub workspace: PathBuf,
    pub state_root: Option<PathBuf>,
    pub session_dir: Option<PathBuf>,
    pub provider: ClientConfig,
    pub provider_profile: Option<String>,
    /// Environment reference names only, including unselected trusted profiles.
    pub credential_references: Vec<String>,
    pub options: ModelOptions,
    pub models: Vec<aicoder_core::model::ModelSpec>,
    pub system_prompt: Option<String>,
    pub execution: ResolvedExecution,
    pub context: ResolvedContext,
    pub resources: crate::resources::ResourcePolicy,
    pub packages: crate::packages::PackagePolicy,
    pub code_mode: crate::code_mode::CodeModePolicy,
    pub sources: BTreeMap<String, ConfigSource>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedExecution {
    pub tool_timeout_ms: u64,
    pub max_rounds: usize,
    pub stream: bool,
    pub budget: RunBudget,
    pub events: aicoder_core::events::EventDeliveryConfig,
}
impl ResolvedExecution {
    pub fn core_config(&self) -> TurnExecutionConfig {
        TurnExecutionConfig {
            max_rounds: self.max_rounds,
            stream: self.stream,
            budget: self.budget.clone(),
            events: self.events.clone(),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedContext {
    pub tokenizer: TokenizerMode,
    pub strategy: ContextStrategy,
    pub summary_output_tokens: u32,
    pub window_tokens: Option<usize>,
    pub reserved_output_tokens: usize,
    pub preserve_recent_tokens: usize,
}
impl ResolvedContext {
    pub fn core_config(&self) -> Option<ContextWindowConfig> {
        self.window_tokens
            .map(|max_context_tokens| ContextWindowConfig {
                max_context_tokens,
                reserved_output_tokens: self.reserved_output_tokens,
                preserve_recent_tokens: self.preserve_recent_tokens,
            })
    }
}
struct Layer {
    settings: Settings,
    path: PathBuf,
}
struct Selector<'a> {
    env: &'a dyn Environment,
    explicit: &'a Settings,
    user: Option<&'a Layer>,
    project: Option<&'a Layer>,
    sources: BTreeMap<String, ConfigSource>,
}
impl Selector<'_> {
    fn choose<T: Clone>(
        &mut self,
        key: &str,
        explicit: &Setting<T>,
        project: Option<&Setting<T>>,
        user: Option<&Setting<T>>,
        environment: impl FnOnce() -> Result<Option<(Setting<T>, String)>>,
    ) -> Result<Setting<T>> {
        let (value, source) = if !explicit.is_inherit() {
            (explicit.clone(), ConfigSource::Explicit)
        } else if let Some((value, variable)) = environment()? {
            (value, self.env.source(&variable))
        } else if let Some(value) = project.filter(|value| !value.is_inherit()) {
            (
                value.clone(),
                ConfigSource::Project {
                    path: self.project.unwrap().path.clone(),
                },
            )
        } else if let Some(value) = user.filter(|value| !value.is_inherit()) {
            (
                value.clone(),
                ConfigSource::User {
                    path: self.user.unwrap().path.clone(),
                },
            )
        } else {
            (Setting::Inherit, ConfigSource::Default)
        };
        self.sources.insert(key.into(), source);
        Ok(value)
    }
}
fn optional<T>(value: Setting<T>, default: Option<T>) -> Option<T> {
    match value {
        Setting::Value(value) => Some(value),
        Setting::Clear => None,
        Setting::Inherit => default,
    }
}
fn required<T>(key: &str, value: Setting<T>, default: T) -> Result<T> {
    match value {
        Setting::Value(value) => Ok(value),
        Setting::Inherit => Ok(default),
        Setting::Clear => bail!("{key} cannot be null"),
    }
}
fn env_value(env: &dyn Environment, name: &str) -> Result<Option<String>> {
    env.value(name)
        .map(|value| value.filter(|value| !value.trim().is_empty()))
        .map_err(|_| anyhow::anyhow!("Cannot read environment setting {name}"))
}
fn environment_aliases(env: &dyn Environment, names: &[&str]) -> Result<Option<(String, String)>> {
    let mut file_fallback = None;
    for name in names {
        if let Some(value) = env_value(env, name)? {
            if matches!(env.source(name), ConfigSource::EnvironmentFile { .. }) {
                if file_fallback.is_none() {
                    file_fallback = Some((value, name.to_string()));
                }
            } else {
                return Ok(Some((value, name.to_string())));
            }
        }
    }
    Ok(file_fallback)
}
fn env_setting<T: std::str::FromStr>(
    env: &dyn Environment,
    name: &str,
) -> Result<Option<(Setting<T>, String)>> {
    env_value(env, name)?
        .map(|value| {
            let value = if value == "null" {
                Setting::Clear
            } else {
                Setting::Value(
                    value
                        .parse()
                        .map_err(|_| anyhow::anyhow!("Invalid environment setting {name}"))?,
                )
            };
            Ok((value, name.into()))
        })
        .transpose()
}
fn env_code_tools(env: &dyn Environment) -> Result<Option<(Setting<Vec<String>>, String)>> {
    env_code_list(env, "AICODER_CODE_MODE_TOOLS")
}
fn env_code_list(
    env: &dyn Environment,
    name: &str,
) -> Result<Option<(Setting<Vec<String>>, String)>> {
    env_value(env, name)?
        .map(|value| {
            ensure!(
                value.len() <= 32768,
                "Code Mode selection environment budget exceeded"
            );
            let selected = if value == "null" {
                Setting::Clear
            } else {
                Setting::Value(serde_json::from_str(&value).map_err(|_| {
                    anyhow::anyhow!("Invalid environment setting {name}; expected a JSON array")
                })?)
            };
            Ok((selected, name.into()))
        })
        .transpose()
}
fn env_packages(env: &dyn Environment) -> Result<Option<(Setting<Vec<String>>, String)>> {
    let name = "AICODER_PACKAGES";
    env_value(env, name)?
        .map(|value| {
            ensure!(
                value.len() <= 8192,
                "package environment selection byte limit exceeded"
            );
            let setting = if value == "null" {
                Setting::Clear
            } else {
                Setting::Value(serde_json::from_str(&value).map_err(|_| {
                    anyhow::anyhow!("Invalid environment setting {name}; expected a JSON array")
                })?)
            };
            Ok((setting, name.into()))
        })
        .transpose()
}
fn path_value(env: &dyn Environment, name: &str) -> Result<Option<PathBuf>> {
    let value = env.path(name)?;
    ensure!(
        value
            .as_ref()
            .is_none_or(|path| !path.as_os_str().is_empty()),
        "{name} cannot be empty"
    );
    Ok(value)
}
fn absolute(base: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}
fn read_file(path: &Path, required: bool) -> Result<Option<(PathBuf, SettingsFile)>> {
    let Some((canonical, bytes)) = crate::files::read_bounded(path, MAX_SETTINGS_BYTES, required)?
    else {
        return Ok(None);
    };
    let document: SettingsFile = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
            "Invalid settings file {} at line {}, column {}",
            canonical.display(),
            error.line(),
            error.column()
        )
    })?;
    ensure!(
        document.version == SETTINGS_VERSION,
        "unsupported settings version in {}",
        canonical.display()
    );
    Ok(Some((canonical, document)))
}
pub(crate) fn resolve(
    workspace: &Path,
    overrides: &RuntimeOverrides,
    discovery: &ConfigDiscovery,
    env: Arc<dyn Environment>,
) -> Result<RuntimeConfig> {
    let workspace = workspace
        .canonicalize()
        .context("Cannot resolve runtime workspace")?;
    ensure!(workspace.is_dir(), "runtime workspace is not a directory");
    let (state_root, state_source) = match path_value(env.as_ref(), "AICODER_HOME")? {
        Some(path) => (Some(absolute(&workspace, path)), env.source("AICODER_HOME")),
        None => match path_value(env.as_ref(), "HOME")? {
            Some(path) => (
                Some(absolute(&workspace, path).join(".aicoder")),
                env.source("HOME"),
            ),
            None => (None, ConfigSource::Default),
        },
    };
    let user = if discovery.enabled {
        match &discovery.user_file {
            Some(path) => read_file(&absolute(&workspace, path.clone()), true)?,
            None => match &state_root {
                Some(root) => read_file(&root.join("settings.json"), false)?,
                None => None,
            },
        }
    } else {
        None
    };
    let project = if discovery.enabled {
        let path = discovery
            .project_file
            .clone()
            .map(|path| absolute(&workspace, path))
            .unwrap_or_else(|| workspace.join(".aicoder/settings.json"));
        let candidate = path.canonicalize().ok();
        if user
            .as_ref()
            .is_some_and(|(path, _)| Some(path) == candidate.as_ref())
        {
            None
        } else {
            read_file(&path, discovery.project_file.is_some())?
        }
    } else {
        None
    };
    if let Some((_, file)) = &project {
        ensure!(
            file.profiles.is_empty(),
            "project settings cannot define provider profiles; define routing in the user file"
        );
        ensure!(
            file.session_dir.is_inherit(),
            "project settings cannot choose the session storage path"
        );
    }
    let profiles = user
        .as_ref()
        .map(|(_, file)| file.profiles.clone())
        .unwrap_or_default();
    let user_session = user
        .as_ref()
        .map(|(path, file)| (path.clone(), file.session_dir.clone()));
    let user = user.map(|(path, file)| Layer {
        path,
        settings: file.settings,
    });
    let project = project.map(|(path, file)| Layer {
        path,
        settings: file.settings,
    });
    let mut selector = Selector {
        env: env.as_ref(),
        explicit: &overrides.settings,
        user: user.as_ref(),
        project: project.as_ref(),
        sources: BTreeMap::new(),
    };
    macro_rules! pick {($key:literal,$($field:ident).+,$environment:expr)=>{{selector.choose($key,&selector.explicit.$($field).+,selector.project.map(|layer|&layer.settings.$($field).+),selector.user.map(|layer|&layer.settings.$($field).+),$environment)?}};}
    let profile_name = if overrides.provider.is_some() {
        selector
            .sources
            .insert("provider_profile".into(), ConfigSource::Explicit);
        None
    } else {
        optional(
            pick!(
                "provider_profile",
                provider_profile,
                || env_setting::<String>(env.as_ref(), "AICODER_PROFILE")
            ),
            None,
        )
    };
    let selected_profile = if overrides.provider.is_some() {
        None
    } else {
        profile_name
            .as_ref()
            .map(|name| {
                profiles
                    .get(name)
                    .context("Selected provider profile does not exist")
            })
            .transpose()?
    };
    let profile_source = if selected_profile.is_some() {
        ConfigSource::User {
            path: user.as_ref().unwrap().path.clone(),
        }
    } else {
        ConfigSource::Default
    };
    let low_profile = profile_variables(selected_profile, false)?;
    let high_profile = profile_variables(overrides.provider.as_ref(), true)?;
    let provider_sources = std::cell::RefCell::new(BTreeMap::new());
    let lookup = |name: &str| -> Result<Option<String>> {
        if matches!(
            name,
            "AICODER_API_KEY" | "OPENAI_API_KEY" | "DEEPSEEK_API_KEY"
        ) {
            return Ok(env
                .present(name)?
                .then(|| "credential-is-present".to_string()));
        }
        let value = if let Some(value) = high_profile.get(name) {
            (Some(value.clone()), ConfigSource::Explicit)
        } else {
            let names: Vec<&str> = if name == "AICODER_API_BASE" {
                vec!["AICODER_API_BASE", "OPENAI_API_BASE"]
            } else {
                vec![name]
            };
            let from_env = environment_aliases(env.as_ref(), &names)?;
            if let Some((value, variable)) = from_env {
                (Some(value), env.source(&variable))
            } else {
                match low_profile.get(name) {
                    Some(value) => (Some(value.clone()), profile_source.clone()),
                    None => (None, ConfigSource::Default),
                }
            }
        };
        provider_sources
            .borrow_mut()
            .insert(name.to_string(), value.1);
        Ok(value.0)
    };
    let probe = ClientConfig::descriptor_from_environment(Some("runtime-probe"), lookup)?;
    let deepseek = probe.compatibility != CompatibilityProfile::Generic
        || probe.api_key_env.as_deref() == Some("DEEPSEEK_API_KEY")
        || url::Url::parse(&probe.base_url)
            .ok()
            .is_some_and(|url| url.host_str() == Some("api.deepseek.com"));
    let model_env = || -> Result<Option<(Setting<String>, String)>> {
        let names = if deepseek {
            &["AICODER_MODEL", "OPENAI_MODEL", "DEEPSEEK_MODEL"][..]
        } else {
            &["AICODER_MODEL", "OPENAI_MODEL"][..]
        };
        Ok(
            environment_aliases(env.as_ref(), names)?.map(|(value, name)| {
                (
                    if value == "null" {
                        Setting::Clear
                    } else {
                        Setting::Value(value)
                    },
                    name,
                )
            }),
        )
    };
    let chosen_model = if let Some(model) = overrides
        .provider
        .as_ref()
        .and_then(|profile| profile.model.clone())
        .filter(|_| overrides.settings.model.is_inherit())
    {
        selector
            .sources
            .insert("model".into(), ConfigSource::Explicit);
        Setting::Value(model)
    } else {
        pick!("model", model, model_env)
    };
    let model = match chosen_model {
        Setting::Value(model) => Some(model),
        Setting::Clear => None,
        Setting::Inherit => {
            let profile = overrides.provider.as_ref().or(selected_profile);
            if let Some(model) = profile.and_then(|profile| profile.model.clone()) {
                selector.sources.insert(
                    "model".into(),
                    if overrides.provider.is_some() {
                        ConfigSource::Explicit
                    } else {
                        profile_source.clone()
                    },
                );
                Some(model)
            } else {
                None
            }
        }
    };
    // Model layering is already resolved; hide environment aliases when null explicitly clears it.
    let mut provider = ClientConfig::descriptor_from_environment(model.as_deref(), |name| {
        if matches!(name, "AICODER_MODEL" | "OPENAI_MODEL" | "DEEPSEEK_MODEL") {
            Ok(None)
        } else {
            lookup(name)
        }
    })?;
    provider.api_key.clear();
    for (field, variable) in [
        ("provider.base_url", "AICODER_API_BASE"),
        ("provider.provider_id", "AICODER_PROVIDER_ID"),
        ("provider.compatibility", "AICODER_COMPATIBILITY"),
        ("provider.proxy", "AICODER_PROXY"),
        ("provider.timeout", "AICODER_HTTP_TIMEOUT"),
        ("provider.connect_timeout", "AICODER_CONNECT_TIMEOUT"),
        ("provider.max_retries", "AICODER_MAX_RETRIES"),
        ("provider.max_response_bytes", "AICODER_MAX_RESPONSE_BYTES"),
        ("provider.stream_usage", "AICODER_STREAM_USAGE"),
        ("provider.api_key_env", "AICODER_API_KEY_ENV"),
    ] {
        selector.sources.insert(
            field.into(),
            provider_sources
                .borrow()
                .get(variable)
                .cloned()
                .unwrap_or(ConfigSource::Default),
        );
    }
    selector.sources.insert("state_root".into(), state_source);
    if matches!(
        selector.sources.get("provider.api_key_env"),
        Some(ConfigSource::Default)
    ) && let Some(variable) = &provider.api_key_env
        && env.present(variable)?
    {
        selector
            .sources
            .insert("provider.api_key_env".into(), env.source(variable));
    }
    let options = ModelOptions {
        reasoning_effort: optional(
            pick!("options.reasoning_effort", options.reasoning_effort, || {
                env_setting::<aicoder_core::model::ReasoningEffort>(
                    env.as_ref(),
                    "AICODER_REASONING_EFFORT",
                )
            }),
            None,
        ),
        max_output_tokens: optional(
            pick!(
                "options.max_output_tokens",
                options.max_output_tokens,
                || env_setting::<u32>(env.as_ref(), "AICODER_MAX_OUTPUT_TOKENS")
            ),
            Some(4096),
        ),
        temperature: optional(
            pick!(
                "options.temperature",
                options.temperature,
                || env_setting::<f32>(env.as_ref(), "AICODER_TEMPERATURE")
            ),
            None,
        ),
        top_p: optional(
            pick!("options.top_p", options.top_p, || env_setting::<f32>(
                env.as_ref(),
                "AICODER_TOP_P"
            )),
            None,
        ),
        seed: optional(
            pick!("options.seed", options.seed, || env_setting::<i64>(
                env.as_ref(),
                "AICODER_SEED"
            )),
            None,
        ),
        stop: optional(pick!("options.stop", options.stop, || Ok(None)), None),
        response_format: optional(
            pick!("options.response_format", options.response_format, || Ok(
                None
            )),
            None,
        ),
    };
    ensure!(
        options.max_output_tokens != Some(0),
        "max_output_tokens must be positive or null"
    );
    ensure!(
        options
            .temperature
            .is_none_or(|value| value.is_finite() && (0.0..=2.0).contains(&value)),
        "temperature must be between 0 and 2"
    );
    ensure!(
        options
            .top_p
            .is_none_or(|value| value.is_finite() && (0.0..=1.0).contains(&value)),
        "top_p must be between 0 and 1"
    );
    let execution = ResolvedExecution {
        tool_timeout_ms: required(
            "tool_timeout_ms",
            pick!(
                "execution.tool_timeout_ms",
                execution.tool_timeout_ms,
                || env_setting::<u64>(env.as_ref(), "AICODER_TOOL_TIMEOUT_MS")
            ),
            30_000,
        )?,
        max_rounds: required(
            "max_rounds",
            pick!(
                "execution.max_rounds",
                execution.max_rounds,
                || env_setting::<usize>(env.as_ref(), "AICODER_MAX_ROUNDS")
            ),
            8,
        )?,
        stream: required(
            "stream",
            pick!(
                "execution.stream",
                execution.stream,
                || env_setting::<bool>(env.as_ref(), "AICODER_STREAM")
            ),
            true,
        )?,
        budget: RunBudget {
            max_auxiliary_calls: optional(
                pick!(
                    "execution.max_auxiliary_calls",
                    execution.max_auxiliary_calls,
                    || env_setting::<usize>(env.as_ref(), "AICODER_MAX_AUXILIARY_CALLS")
                ),
                None,
            ),
            max_tool_calls: optional(
                pick!("execution.max_tool_calls", execution.max_tool_calls, || {
                    env_setting::<usize>(env.as_ref(), "AICODER_MAX_TOOL_CALLS")
                }),
                None,
            ),
            max_tokens: optional(
                pick!(
                    "execution.max_tokens",
                    execution.max_tokens,
                    || env_setting::<u64>(env.as_ref(), "AICODER_TOKEN_BUDGET")
                ),
                None,
            ),
            max_duration: optional(
                pick!(
                    "execution.max_duration_ms",
                    execution.max_duration_ms,
                    || env_setting::<u64>(env.as_ref(), "AICODER_TIME_BUDGET_MS")
                ),
                None,
            )
            .map(Duration::from_millis),
            max_output_bytes: optional(
                pick!(
                    "execution.max_output_bytes",
                    execution.max_output_bytes,
                    || env_setting::<usize>(env.as_ref(), "AICODER_OUTPUT_BUDGET_BYTES")
                ),
                None,
            ),
        },
        events: required(
            "events",
            pick!("execution.events", execution.events, || Ok(None)),
            Default::default(),
        )?,
    };
    ensure!(execution.max_rounds > 0, "max_rounds must be positive");
    ensure!(
        (1..=86_400_000).contains(&execution.tool_timeout_ms),
        "tool_timeout_ms must be 1..86400000"
    );
    execution.events.validate()?;
    if let Some(duration) = execution.budget.max_duration {
        ensure!(
            std::time::Instant::now().checked_add(duration).is_some(),
            "execution time budget is too large"
        );
    }
    let window_tokens = optional(
        pick!("context.window_tokens", context.window_tokens, || {
            env_setting::<usize>(env.as_ref(), "AICODER_CONTEXT_WINDOW")
        }),
        None,
    );
    let reserved_output_tokens = required(
        "reserved_output_tokens",
        pick!(
            "context.reserved_output_tokens",
            context.reserved_output_tokens,
            || env_setting::<usize>(env.as_ref(), "AICODER_RESERVED_OUTPUT_TOKENS")
        ),
        options.max_output_tokens.unwrap_or(4096) as usize,
    )?;
    let effective_reservation =
        reserved_output_tokens.max(options.max_output_tokens.unwrap_or(0) as usize);
    let preserve_default = window_tokens.map_or(8192, |window| {
        8192.min(
            window
                .saturating_sub(effective_reservation)
                .saturating_sub(1),
        )
    });
    let preserve_recent_tokens = required(
        "preserve_recent_tokens",
        pick!(
            "context.preserve_recent_tokens",
            context.preserve_recent_tokens,
            || env_setting::<usize>(env.as_ref(), "AICODER_PRESERVE_RECENT_TOKENS")
        ),
        preserve_default,
    )?;
    let strategy = required(
        "context.strategy",
        pick!("context.strategy", context.strategy, || env_setting::<
            ContextStrategy,
        >(
            env.as_ref(),
            "AICODER_CONTEXT_STRATEGY"
        )),
        ContextStrategy::Prune,
    )?;
    let summary_output_tokens = required(
        "context.summary_output_tokens",
        pick!(
            "context.summary_output_tokens",
            context.summary_output_tokens,
            || env_setting::<u32>(env.as_ref(), "AICODER_SUMMARY_OUTPUT_TOKENS")
        ),
        1024,
    )?;
    ensure!(
        (1..=8192).contains(&summary_output_tokens),
        "summary_output_tokens must be 1..8192"
    );
    ensure!(
        strategy != ContextStrategy::Semantic || window_tokens.is_some(),
        "semantic context requires an explicit context window"
    );
    let tokenizer = required(
        "context.tokenizer",
        pick!("context.tokenizer", context.tokenizer, || env_setting::<
            TokenizerMode,
        >(
            env.as_ref(),
            "AICODER_TOKENIZER"
        )),
        TokenizerMode::Heuristic,
    )?;
    #[cfg(not(feature = "tokenizer-tiktoken"))]
    ensure!(
        matches!(tokenizer, TokenizerMode::Heuristic | TokenizerMode::Auto),
        "explicit BPE tokenizers require the tokenizer-tiktoken Cargo feature"
    );
    let context = ResolvedContext {
        tokenizer,
        strategy,
        summary_output_tokens,
        window_tokens,
        reserved_output_tokens,
        preserve_recent_tokens,
    };
    if let Some(config) = context.core_config() {
        config.validate()?;
        ensure!(
            config.preserve_recent_tokens
                < config
                    .max_context_tokens
                    .saturating_sub(effective_reservation),
            "preserve_recent_tokens must fit after the effective output reservation"
        );
        ensure!(
            options
                .max_output_tokens
                .is_none_or(|tokens| (tokens as usize) < config.max_context_tokens),
            "max_output_tokens must fit in the context window"
        );
    }
    let system_prompt = optional(
        pick!("system_prompt", system_prompt, || env_setting::<String>(
            env.as_ref(),
            "AICODER_SYSTEM_PROMPT"
        )),
        Some(DEFAULT_SYSTEM_PROMPT.into()),
    );
    let (session_dir, source) = if !overrides.session_dir.is_inherit() {
        (
            optional(overrides.session_dir.clone(), None).map(|path| absolute(&workspace, path)),
            ConfigSource::Explicit,
        )
    } else if let Some(path) = path_value(env.as_ref(), "AICODER_SESSION_DIR")? {
        (
            Some(absolute(&workspace, path)),
            env.source("AICODER_SESSION_DIR"),
        )
    } else if let Some((path, value)) = user_session.filter(|(_, setting)| !setting.is_inherit()) {
        (
            optional(value, None).map(|value| absolute(path.parent().unwrap(), value)),
            ConfigSource::User { path },
        )
    } else {
        (
            state_root.as_ref().map(|root| root.join("sessions")),
            ConfigSource::Default,
        )
    };
    selector.sources.insert("session_dir".into(), source);
    let defaults = crate::resources::ResourcePolicy::default();
    let resources = crate::resources::ResourcePolicy {
        enabled: required(
            "resources.enabled",
            pick!(
                "resources.enabled",
                resources.enabled,
                || env_setting::<bool>(env.as_ref(), "AICODER_RESOURCES")
            ),
            defaults.enabled,
        )?,
        global: required(
            "resources.global",
            pick!(
                "resources.global",
                resources.global,
                || env_setting::<bool>(env.as_ref(), "AICODER_GLOBAL_CONTEXT")
            ),
            defaults.global,
        )?,
        parents: required(
            "resources.parents",
            pick!(
                "resources.parents",
                resources.parents,
                || env_setting::<bool>(env.as_ref(), "AICODER_PARENT_CONTEXT")
            ),
            defaults.parents,
        )?,
        nested: required(
            "resources.nested",
            pick!(
                "resources.nested",
                resources.nested,
                || env_setting::<bool>(env.as_ref(), "AICODER_NESTED_CONTEXT")
            ),
            defaults.nested,
        )?,
        instructions: required(
            "resources.instructions",
            pick!("resources.instructions", resources.instructions, || {
                env_setting::<bool>(env.as_ref(), "AICODER_INSTRUCTIONS")
            }),
            defaults.instructions,
        )?,
        system: required(
            "resources.system",
            pick!(
                "resources.system",
                resources.system,
                || env_setting::<bool>(env.as_ref(), "AICODER_SYSTEM_FILES")
            ),
            defaults.system,
        )?,
        skills: required(
            "resources.skills",
            pick!(
                "resources.skills",
                resources.skills,
                || env_setting::<bool>(env.as_ref(), "AICODER_SKILLS")
            ),
            defaults.skills,
        )?,
        prompts: required(
            "resources.prompts",
            pick!(
                "resources.prompts",
                resources.prompts,
                || env_setting::<bool>(env.as_ref(), "AICODER_PROMPTS")
            ),
            defaults.prompts,
        )?,
        max_file_bytes: required(
            "resources.max_file_bytes",
            pick!("resources.max_file_bytes", resources.max_file_bytes, || Ok(
                None
            )),
            defaults.max_file_bytes,
        )?,
        max_total_bytes: required(
            "resources.max_total_bytes",
            pick!(
                "resources.max_total_bytes",
                resources.max_total_bytes,
                || Ok(None)
            ),
            defaults.max_total_bytes,
        )?,
        max_scan_bytes: required(
            "resources.max_scan_bytes",
            pick!("resources.max_scan_bytes", resources.max_scan_bytes, || Ok(
                None
            )),
            defaults.max_scan_bytes,
        )?,
        max_files: required(
            "resources.max_files",
            pick!("resources.max_files", resources.max_files, || Ok(None)),
            defaults.max_files,
        )?,
        max_entries: required(
            "resources.max_entries",
            pick!("resources.max_entries", resources.max_entries, || Ok(None)),
            defaults.max_entries,
        )?,
        max_depth: required(
            "resources.max_depth",
            pick!("resources.max_depth", resources.max_depth, || Ok(None)),
            defaults.max_depth,
        )?,
        excluded_directories: required(
            "resources.excluded_directories",
            pick!(
                "resources.excluded_directories",
                resources.excluded_directories,
                || Ok(None)
            ),
            defaults.excluded_directories,
        )?,
    };
    resources.validate()?;
    let packages = crate::packages::PackagePolicy {
        enabled: required(
            "packages.enabled",
            pick!(
                "packages.enabled",
                packages.enabled,
                || env_setting::<bool>(env.as_ref(), "AICODER_PACKAGES_ENABLED")
            ),
            true,
        )?,
        selected: optional(
            pick!("packages.selected", packages.selected, || env_packages(
                env.as_ref()
            )),
            None,
        ),
    };
    packages.validate()?;
    let code_mode = crate::code_mode::CodeModePolicy {
        model_profiles: optional(
            pick!("code_mode.model_profiles", code_mode.model_profiles, || {
                env_code_list(env.as_ref(), "AICODER_MODEL_PROFILES")
            }),
            Some(Vec::new()),
        )
        .unwrap_or_default(),
        only: required(
            "code_mode.only",
            pick!("code_mode.only", code_mode.only, || env_setting::<bool>(
                env.as_ref(),
                "AICODER_CODE_MODE_ONLY"
            )),
            false,
        )?,
        enabled: required(
            "code_mode.enabled",
            pick!(
                "code_mode.enabled",
                code_mode.enabled,
                || env_setting::<bool>(env.as_ref(), "AICODER_CODE_MODE")
            ),
            false,
        )?,
        tools: optional(
            pick!("code_mode.tools", code_mode.tools, || env_code_tools(
                env.as_ref()
            )),
            None,
        ),
    };
    code_mode.validate()?;
    #[cfg(feature = "auxiliary-models")]
    let classifier_routes = {
        let mut classifier_routes = Vec::new();
        let mut classifier_ids = std::collections::BTreeSet::new();
        for name in &code_mode.model_profiles {
            let profile = profiles
                .get(name)
                .context("selected auxiliary profile is not defined in trusted user settings")?;
            profile_variables(Some(profile), false)?;
            ensure!(
                !profile.classifiers.is_empty() && profile.classifiers.len() <= 64,
                "auxiliary profile needs a bounded classifier catalog"
            );
            let provider = profile.provider_id.clone().unwrap_or_else(|| name.clone());
            for model in &profile.classifiers {
                model.validate()?;
                ensure!(
                    model.provider == provider
                        && classifier_ids.insert((model.provider.clone(), model.id.clone())),
                    "duplicate or mismatched classifier route"
                );
            }
            let route = ClientConfig {
                provider_id: provider,
                base_url: profile.base_url.clone(),
                api_key_env: Some(profile.api_key_env.clone()),
                model: profile.classifiers[0].id.clone(),
                timeout: profile.timeout.unwrap_or(30),
                connect_timeout: profile.connect_timeout.unwrap_or(5),
                max_retries: 0,
                proxy: profile.proxy.as_deref().unwrap_or("auto").parse()?,
                max_response_bytes: profile.max_response_bytes.unwrap_or(2 * 1024 * 1024),
                ..Default::default()
            };
            classifier_routes.push(crate::code_mode::ClassifierRoute {
                config: route,
                models: profile.classifiers.clone(),
                images: profile.classifier_image_input,
            });
        }
        classifier_routes
    };
    let mut credential_references = profiles
        .values()
        .map(|profile| profile.api_key_env.clone())
        .chain(provider.api_key_env.iter().cloned())
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
        .collect::<Vec<_>>();
    credential_references.sort();
    credential_references.dedup();
    let declared = overrides
        .provider
        .as_ref()
        .or(selected_profile)
        .map(|p| &p.models);
    ensure!(
        declared.is_none_or(|m| m.len() <= 128),
        "trusted model metadata limit exceeded"
    );
    let models = declared.cloned().unwrap_or_default();
    let mut identities = std::collections::BTreeSet::new();
    for spec in &models {
        spec.validate()?;
        ensure!(
            spec.provider == provider.provider_id,
            "model metadata belongs to another provider route"
        );
        ensure!(
            identities.insert(spec.id.clone()),
            "duplicate model metadata identity"
        );
    }
    selector.sources.insert(
        "provider.models".into(),
        if overrides.provider.is_some() {
            ConfigSource::Explicit
        } else {
            profile_source.clone()
        },
    );
    Ok(RuntimeConfig {
        #[cfg(feature = "auxiliary-models")]
        classifier_routes,
        version: SETTINGS_VERSION,
        workspace,
        state_root,
        session_dir,
        provider,
        provider_profile: profile_name,
        credential_references,
        options,
        models,
        system_prompt,
        execution,
        context,
        resources,
        packages,
        code_mode,
        sources: selector.sources,
    })
}
fn profile_variables(
    profile: Option<&ProviderProfile>,
    explicit: bool,
) -> Result<BTreeMap<String, String>> {
    let Some(profile) = profile else {
        return Ok(BTreeMap::new());
    };
    aicoder_providers::EnvironmentCredential::new(profile.api_key_env.clone())?;
    ensure!(
        profile
            .provider_id
            .as_ref()
            .is_none_or(|value| !value.trim().is_empty()),
        "profile provider_id must not be empty"
    );
    if let Some(proxy) = &profile.proxy
        && let ProxyMode::Explicit {
            username, password, ..
        } = proxy.parse::<ProxyMode>()?
    {
        ensure!(
            explicit || username.is_none() && password.is_none(),
            "proxy credentials must come from environment rather than settings files"
        );
    }
    let mut values = BTreeMap::from([
        ("AICODER_API_BASE".into(), profile.base_url.clone()),
        ("AICODER_API_KEY_ENV".into(), profile.api_key_env.clone()),
    ]);
    macro_rules! add {
        ($field:ident,$name:literal) => {
            if let Some(value) = &profile.$field {
                values.insert($name.into(), value.to_string());
            }
        };
    }
    add!(provider_id, "AICODER_PROVIDER_ID");
    add!(proxy, "AICODER_PROXY");
    add!(timeout, "AICODER_HTTP_TIMEOUT");
    add!(connect_timeout, "AICODER_CONNECT_TIMEOUT");
    add!(max_retries, "AICODER_MAX_RETRIES");
    add!(max_response_bytes, "AICODER_MAX_RESPONSE_BYTES");
    add!(stream_usage, "AICODER_STREAM_USAGE");
    if let Some(mode) = profile.compatibility {
        values.insert(
            "AICODER_COMPATIBILITY".into(),
            serde_json::to_value(mode)?.as_str().unwrap().to_string(),
        );
    }
    Ok(values)
}
