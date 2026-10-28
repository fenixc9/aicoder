//! Shared application configuration and agent assembly. Core remains environment/UI independent.
mod config;
mod environment;
mod files;
mod settings;
use aicoder_core::{
    AgentConfig, ModelProvider, PruningContextCompactor, TurnExecutor, TurnExecutorBuilder,
    session::JsonlSessionRepository,
    tools::{BashTool, EditFileTool, GrepTool, ReadFileTool, ToolRegistry, WriteFileTool},
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
    ConfigDiscovery, ContextSettings, ExecutionSettings, ModelSettings, ProviderProfile,
    RuntimeOverrides, Setting, Settings,
};
use std::{path::Path, sync::Arc};

/// Immutable effective settings plus request-time credentials. Reload creates a new snapshot.
pub struct Runtime {
    config: RuntimeConfig,
    environment: Arc<dyn Environment>,
    overrides: RuntimeOverrides,
    discovery: ConfigDiscovery,
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
    pub fn session_dir(&self) -> Result<&Path> {
        self.config.session_dir.as_deref().context("Session storage is disabled or has no path; set AICODER_HOME, HOME or an explicit session directory")
    }
    pub fn sessions(&self) -> Result<JsonlSessionRepository> {
        JsonlSessionRepository::new(self.session_dir()?)
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
    pub fn client_with_credentials(
        &self,
        credentials: Arc<dyn CredentialProvider>,
    ) -> Result<ChatClient> {
        ChatClient::with_credentials(self.config.provider.clone(), credentials)
    }
    /// Default capabilities are assembled here; callers can override tools, approval and policies.
    pub fn default_registry(&self) -> Result<ToolRegistry> {
        let mut registry = ToolRegistry::default();
        registry.register(ReadFileTool)?;
        registry.register(WriteFileTool)?;
        registry.register(EditFileTool)?;
        registry.register(BashTool::excluding_environment(
            self.config.credential_references.clone(),
        ))?;
        registry.register(GrepTool)?;
        Ok(registry)
    }
    pub fn executor_builder(
        &self,
        provider: Arc<dyn ModelProvider>,
    ) -> Result<TurnExecutorBuilder> {
        let mut builder = TurnExecutor::builder_from_shared(provider)
            .workspace(&self.config.workspace)
            .registry(self.default_registry()?)
            .config(self.config.execution.core_config());
        if let Some(context) = self.config.context.core_config() {
            builder = builder.context_compactor(PruningContextCompactor::new(context)?);
        }
        Ok(builder)
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
