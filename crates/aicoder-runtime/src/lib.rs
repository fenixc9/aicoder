//! Shared application configuration and agent assembly. Core remains environment/UI independent.
pub mod artifacts;
mod config;
mod environment;
pub mod exports;
mod files;
pub mod resources;
pub mod session;
mod session_client;
mod settings;
pub mod tokenizers;
pub use session_client::SessionModelClient;
pub mod tools;
pub mod verification;
use crate::{session::JsonlSessionRepository, tools::BashTool};
use aicoder_core::{
    AgentConfig, ModelProvider, PruningContextCompactor, TurnExecutor, TurnExecutorBuilder,
    tools::ToolRegistry,
};
use aicoder_providers::{BearerToken, ChatClient, CredentialProvider};
use anyhow::{Context, Result};
use async_trait::async_trait;
pub use config::{
    ConfigSource, DEFAULT_SYSTEM_PROMPT, MAX_SETTINGS_BYTES, ResolvedContext, ResolvedExecution,
    RuntimeConfig, SETTINGS_VERSION,
};
pub use environment::{Environment, ProcessEnvironment, environment_file};
pub use settings::{
    ConfigDiscovery, ContextSettings, ContextStrategy, ExecutionSettings, ModelSettings,
    ProviderProfile, RuntimeOverrides, Setting, Settings, TokenizerMode,
};
use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

/// Immutable effective settings plus request-time credentials. Reload creates a new snapshot.
pub struct Runtime {
    config: RuntimeConfig,
    environment: Arc<dyn Environment>,
    overrides: RuntimeOverrides,
    discovery: ConfigDiscovery,
    resources: OnceLock<Result<Arc<resources::ResourceSnapshot>, String>>,
    global_resources: OnceLock<Result<Arc<resources::ResourceSnapshot>, String>>,
}
impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}
impl Runtime {
    pub fn load(workspace: impl AsRef<Path>, overrides: RuntimeOverrides) -> Result<Self> {
        Self::load_with_environment(
            workspace,
            overrides,
            ConfigDiscovery::default(),
            Arc::new(ProcessEnvironment),
        )
    }
    pub fn load_with_environment(
        workspace: impl AsRef<Path>,
        overrides: RuntimeOverrides,
        discovery: ConfigDiscovery,
        environment: Arc<dyn Environment>,
    ) -> Result<Self> {
        let config = config::resolve(
            workspace.as_ref(),
            &overrides,
            &discovery,
            environment.clone(),
        )?;
        Ok(Self {
            config,
            environment,
            overrides,
            discovery,
            resources: OnceLock::new(),
            global_resources: OnceLock::new(),
        })
    }
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }
    pub fn reload(&self) -> Result<Self> {
        Self::load_with_environment(
            &self.config.workspace,
            self.overrides.clone(),
            self.discovery.clone(),
            self.environment.clone(),
        )
    }
    pub fn global_resources(&self) -> Result<Arc<resources::ResourceSnapshot>> {
        self.global_resources
            .get_or_init(|| {
                resources::ResourceSnapshot::load_global(&self.config)
                    .map(Arc::new)
                    .map_err(|error| format!("{error:#}"))
            })
            .clone()
            .map_err(anyhow::Error::msg)
    }
    pub fn resources(&self) -> Result<Arc<resources::ResourceSnapshot>> {
        self.resources
            .get_or_init(|| {
                self.global_resources()
                    .and_then(|global| {
                        resources::ResourceSnapshot::load_with_global(&self.config, &global)
                    })
                    .map(Arc::new)
                    .map_err(|error| format!("{error:#}"))
            })
            .clone()
            .map_err(anyhow::Error::msg)
    }
    /// Refresh resources only; current model/options/storage and existing agents remain unchanged.
    pub fn reload_resources(&self) -> Result<Self> {
        let next = Self {
            config: self.config.clone(),
            environment: self.environment.clone(),
            overrides: self.overrides.clone(),
            discovery: self.discovery.clone(),
            resources: OnceLock::new(),
            global_resources: OnceLock::new(),
        };
        next.resources()?;
        Ok(next)
    }
    pub fn prepare_prompt(&self, prompt: &str) -> Result<resources::PreparedPrompt> {
        self.resources()?.prepare_prompt(prompt)
    }
    pub fn session_dir(&self) -> Result<&Path> {
        self.config.session_dir.as_deref().context("Session storage is disabled or has no path; set AICODER_HOME, HOME or an explicit session directory")
    }
    pub fn sessions(&self) -> Result<JsonlSessionRepository> {
        JsonlSessionRepository::new(self.session_dir()?)
    }
    /// Workspace-scoped artifact storage under the trusted session root, lazily created.
    pub fn artifact_store_for(
        &self,
        workspace: &Path,
    ) -> Result<Option<Arc<artifacts::FileArtifactStore>>> {
        let Some(root) = &self.config.session_dir else {
            return Ok(None);
        };
        let workspace = workspace.canonicalize()?;
        let mut hash = 0xcbf29ce484222325u64;
        for byte in workspace.as_os_str().as_encoded_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        // Footer validation compares the full canonical workspace even if namespace hashes collide.
        Ok(Some(Arc::new(artifacts::FileArtifactStore::new(
            root.join("artifacts").join(format!("{hash:016x}")),
            &workspace,
            Default::default(),
        )?)))
    }
    pub fn credential_provider(&self) -> Arc<dyn CredentialProvider> {
        Arc::new(RuntimeCredential {
            environment: self.environment.clone(),
            variable: self
                .config
                .provider
                .api_key_env
                .clone()
                .expect("resolved credential reference"),
            provider: self.config.provider.provider_id.clone(),
        })
    }
    pub fn client(&self) -> Result<ChatClient> {
        ChatClient::with_credentials(self.config.provider.clone(), self.credential_provider())
    }
    /// Opt into model selection within this fixed host-selected protocol/credential route.
    pub fn session_client(&self) -> Result<SessionModelClient> {
        Ok(SessionModelClient::new(self.client()?))
    }
    pub fn client_with_credentials(
        &self,
        credentials: Arc<dyn CredentialProvider>,
    ) -> Result<ChatClient> {
        ChatClient::with_credentials(self.config.provider.clone(), credentials)
    }
    /// Default capabilities are assembled here; callers can override tools, approval and policies.
    pub fn default_registry(&self) -> Result<ToolRegistry> {
        self.registry_with_resources(if self.config.resources.enabled {
            Some(self.resources()?)
        } else {
            None
        })
    }
    fn registry_with_resources(
        &self,
        resources: Option<Arc<resources::ResourceSnapshot>>,
    ) -> Result<ToolRegistry> {
        let mut registry = tools::registry_with_shell(BashTool::excluding_environment(
            self.config.credential_references.clone(),
        ))?;
        if self.config.session_dir.is_some() {
            registry.register(aicoder_core::artifacts::ReadArtifactTool)?;
        }
        if let Some(resources) = resources {
            resources.register_tools(&mut registry)?;
        }
        Ok(registry)
    }
    pub fn executor_builder(
        &self,
        provider: Arc<dyn ModelProvider>,
    ) -> Result<TurnExecutorBuilder> {
        let resources = if self.config.resources.enabled {
            Some(self.resources()?)
        } else {
            None
        };
        self.builder_for(provider, &self.config.workspace, resources)
    }
    /// Case/tool workspace differs from invocation settings: isolate parents and reuse frozen globals.
    pub fn executor_builder_for(
        &self,
        provider: Arc<dyn ModelProvider>,
        workspace: &Path,
    ) -> Result<TurnExecutorBuilder> {
        let workspace = workspace.canonicalize()?;
        let resources = if self.config.resources.enabled {
            let mut config = self.config.clone();
            config.workspace = workspace.clone();
            config.resources.parents = false;
            Some(Arc::new(resources::ResourceSnapshot::load_with_global(
                &config,
                self.global_resources()?.as_ref(),
            )?))
        } else {
            None
        };
        self.builder_for(provider, &workspace, resources)
    }
    fn builder_for(
        &self,
        provider: Arc<dyn ModelProvider>,
        workspace: &Path,
        resources: Option<Arc<resources::ResourceSnapshot>>,
    ) -> Result<TurnExecutorBuilder> {
        let mut builder = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(workspace)
            .registry(self.registry_with_resources(resources.clone())?)
            .config(self.config.execution.core_config())
            .dispatcher_config(aicoder_core::tools::DispatcherConfig {
                tool_timeout: std::time::Duration::from_millis(
                    self.config.execution.tool_timeout_ms,
                ),
                ..Default::default()
            });
        builder = builder.token_counter(tokenizers::counter(self.config.context.tokenizer));
        if let Some(store) = self.artifact_store_for(workspace)? {
            builder = builder.artifact_store(store);
        }
        if let Some(resources) = resources {
            builder = builder.input_context(resources);
        }
        if let Some(context) = self.config.context.core_config() {
            builder = match self.config.context.strategy {
                ContextStrategy::Prune => {
                    builder.context_compactor(PruningContextCompactor::new(context)?)
                }
                ContextStrategy::Semantic => {
                    builder.context_compactor(self.semantic_compactor(provider)?)
                }
            };
        }
        Ok(builder)
    }
    fn semantic_compactor(
        &self,
        provider: Arc<dyn ModelProvider>,
    ) -> Result<aicoder_core::summary::SemanticContextCompactor> {
        let window = self
            .config
            .context
            .core_config()
            .context("Set a known context window before semantic compaction")?;
        let mut config = aicoder_core::summary::SummaryConfig::new(window);
        config.output_tokens = self.config.context.summary_output_tokens;
        config.max_input_tokens = window
            .max_context_tokens
            .saturating_sub(config.output_tokens as usize)
            .clamp(512, 32768);
        aicoder_core::summary::SemanticContextCompactor::new(
            provider,
            self.config.provider.model.clone(),
            config,
        )
    }
    pub fn summary_agent(&self, provider: Arc<dyn ModelProvider>) -> Result<aicoder_core::Agent> {
        let compactor = self.semantic_compactor(provider.clone())?;
        Ok(aicoder_core::Agent::new(
            self.executor_builder(provider)?
                .context_compactor(compactor)
                .build()?,
            self.agent_config(),
        ))
    }
    pub fn agent_config(&self) -> AgentConfig {
        AgentConfig {
            model: self.config.provider.model.clone(),
            system_prompt: self.config.system_prompt.clone(),
            options: self.config.options.clone(),
        }
    }
    pub fn state_root(&self) -> Result<&Path> {
        self.config
            .state_root
            .as_deref()
            .context("Cannot determine state directory: set AICODER_HOME or HOME")
    }
}
struct RuntimeCredential {
    environment: Arc<dyn Environment>,
    variable: String,
    provider: String,
}
#[async_trait]
impl CredentialProvider for RuntimeCredential {
    async fn bearer_token(&self) -> Result<BearerToken, aicoder_core::model::ProviderError> {
        let value = self
            .environment
            .value(&self.variable)
            .ok()
            .flatten()
            .ok_or_else(|| {
                aicoder_core::model::ProviderError::new(
                    &self.provider,
                    aicoder_core::model::ProviderErrorKind::Authentication,
                    "Configured credential is missing or unreadable",
                )
            })?;
        BearerToken::new(value)
    }
}
