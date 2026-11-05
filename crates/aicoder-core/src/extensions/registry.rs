use super::{
    hooks::{ContextChain, Registered, invoke},
    *,
};
use crate::{
    AgentEventHandler, AgentRawEventEnvelope, InputContextProvider, TurnExecutionContext,
    context_policy::ContextPolicy, model::*, tools::*,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashSet, sync::Arc};

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionManifest {
    pub api_version: u32,
    pub id: String,
    pub version: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct RegistrationInfo {
    pub extension: String,
    pub kind: String,
    pub name: String,
    pub priority: Option<i32>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ExtensionCatalog {
    pub extensions: Vec<ExtensionManifest>,
    pub registrations: Vec<RegistrationInfo>,
}

#[derive(Clone)]
struct Command {
    owner: String,
    spec: CommandSpec,
    execution: HookSpec,
    callback: Arc<dyn ExtensionCommand>,
}
#[derive(Clone)]
struct Observer {
    priority: i32,
    callback: Arc<dyn AgentEventHandler>,
}
/// An owned, bounded set of native contributions. Installation consumes and freezes it.
#[derive(Clone)]
pub struct Extension {
    manifest: ExtensionManifest,
    tools: Vec<Arc<dyn ExecutableTool>>,
    providers: Vec<(String, Arc<dyn ModelProvider>)>,
    policies: Vec<(String, Arc<dyn ContextPolicy>)>,
    inputs: Vec<Registered<dyn InputHook>>,
    startups: Vec<Registered<dyn BeforeStartHook>>,
    boundaries: Vec<Registered<dyn BoundaryHook>>,
    requests: Vec<RequestRegistration>,
    contexts: Vec<Registered<dyn ContextHook>>,
    guards: Vec<Registered<dyn ToolHook>>,
    call_hooks: Vec<Registered<dyn ToolCallHook>>,
    result_hooks: Vec<Registered<dyn ToolResultHook>>,
    observers: Vec<Observer>,
    commands: Vec<Command>,
    registrations: Vec<RegistrationInfo>,
    bytes: usize,
}
impl Extension {
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Result<Self> {
        let manifest = ExtensionManifest {
            api_version: EXTENSION_API_VERSION,
            id: id.into(),
            version: version.into(),
        };
        name(&manifest.id)?;
        ensure!(
            !manifest.version.is_empty()
                && manifest.version.len() <= 128
                && manifest.version.bytes().all(|b| b.is_ascii_graphic()),
            "invalid extension version"
        );
        let bytes = size(&manifest, 1024)?;
        Ok(Self {
            manifest,
            tools: Vec::new(),
            providers: Vec::new(),
            policies: Vec::new(),
            inputs: Vec::new(),
            startups: Vec::new(),
            boundaries: Vec::new(),
            requests: Vec::new(),
            contexts: Vec::new(),
            guards: Vec::new(),
            call_hooks: Vec::new(),
            result_hooks: Vec::new(),
            observers: Vec::new(),
            commands: Vec::new(),
            registrations: Vec::new(),
            bytes,
        })
    }
    fn register(
        &mut self,
        kind: &str,
        key: &str,
        priority: Option<i32>,
        bytes: usize,
    ) -> Result<()> {
        name(key)?;
        ensure!(
            !self
                .registrations
                .iter()
                .any(|r| r.kind == kind && r.name == key),
            "duplicate extension {kind} registration: {key}"
        );
        ensure!(
            self.registrations.len() < MAX_CONTRIBUTIONS
                && bytes <= MAX_METADATA_BYTES.saturating_sub(self.bytes),
            "extension registration budget exceeded"
        );
        let info = RegistrationInfo {
            extension: self.manifest.id.clone(),
            kind: kind.into(),
            name: key.into(),
            priority,
        };
        let metadata = size(&info, 1024)?;
        ensure!(
            bytes.saturating_add(metadata) <= MAX_METADATA_BYTES.saturating_sub(self.bytes),
            "extension registration metadata budget exceeded"
        );
        self.bytes += bytes + metadata;
        self.registrations.push(info);
        Ok(())
    }
    pub fn tool(&mut self, tool: Arc<dyn ExecutableTool>) -> Result<&mut Self> {
        let definition = tool.definition();
        name(&definition.name)?;
        ensure!(
            definition.parameters.is_object(),
            "extension tool requires a JSON schema object"
        );
        let capability = tool.capability();
        self.register(
            "tool",
            &definition.name,
            None,
            size(&definition, 64 * 1024)?,
        )?;
        self.tools.push(Arc::new(PinnedTool {
            definition,
            capability,
            inner: tool,
        }));
        Ok(self)
    }
    /// Routes are explicit host lookups. Session/model history never selects a provider route.
    pub fn provider(
        &mut self,
        route: impl Into<String>,
        provider: Arc<dyn ModelProvider>,
    ) -> Result<&mut Self> {
        let route = route.into();
        self.register("provider", &route, None, 0)?;
        self.providers.push((route, provider));
        Ok(self)
    }
    pub fn context_policy(
        &mut self,
        key: impl Into<String>,
        policy: Arc<dyn ContextPolicy>,
    ) -> Result<&mut Self> {
        let key = key.into();
        self.register("context_policy", &key, None, 0)?;
        self.policies.push((key, policy));
        Ok(self)
    }
    pub fn context_hook(
        &mut self,
        spec: HookSpec,
        hook: Arc<dyn ContextHook>,
    ) -> Result<&mut Self> {
        spec.validate()?;
        self.register(
            "context_hook",
            &spec.id,
            Some(spec.priority),
            size(&spec, 1024)?,
        )?;
        self.contexts.push(Registered {
            owner: self.manifest.id.clone(),
            spec,
            callback: hook,
        });
        Ok(self)
    }
    pub fn tool_hook(&mut self, spec: HookSpec, hook: Arc<dyn ToolHook>) -> Result<&mut Self> {
        spec.validate()?;
        self.register(
            "tool_hook",
            &spec.id,
            Some(spec.priority),
            size(&spec, 1024)?,
        )?;
        self.guards.push(Registered {
            owner: self.manifest.id.clone(),
            spec,
            callback: hook,
        });
        Ok(self)
    }
    pub fn input_hook(&mut self, spec: HookSpec, hook: Arc<dyn InputHook>) -> Result<&mut Self> {
        spec.validate()?;
        self.register(
            "input_hook",
            &spec.id,
            Some(spec.priority),
            size(&spec, 1024)?,
        )?;
        self.inputs.push(Registered {
            owner: self.manifest.id.clone(),
            spec,
            callback: hook,
        });
        Ok(self)
    }
    pub fn before_start_hook(
        &mut self,
        spec: HookSpec,
        hook: Arc<dyn BeforeStartHook>,
    ) -> Result<&mut Self> {
        spec.validate()?;
        self.register(
            "before_start_hook",
            &spec.id,
            Some(spec.priority),
            size(&spec, 1024)?,
        )?;
        self.startups.push(Registered {
            owner: self.manifest.id.clone(),
            spec,
            callback: hook,
        });
        Ok(self)
    }
    pub fn boundary_hook(
        &mut self,
        spec: HookSpec,
        hook: Arc<dyn BoundaryHook>,
    ) -> Result<&mut Self> {
        spec.validate()?;
        self.register(
            "boundary_hook",
            &spec.id,
            Some(spec.priority),
            size(&spec, 1024)?,
        )?;
        self.boundaries.push(Registered {
            owner: self.manifest.id.clone(),
            spec,
            callback: hook,
        });
        Ok(self)
    }
    pub fn request_hook(
        &mut self,
        spec: HookSpec,
        scope: RequestHookScope,
        hook: Arc<dyn RequestHook>,
    ) -> Result<&mut Self> {
        spec.validate()?;
        let kind = match scope {
            RequestHookScope::Conversation => "request_hook_conversation",
            RequestHookScope::SystemAndConversation => "request_hook_system",
        };
        self.register(kind, &spec.id, Some(spec.priority), size(&spec, 1024)?)?;
        self.requests.push(RequestRegistration {
            hook: Registered {
                owner: self.manifest.id.clone(),
                spec,
                callback: hook,
            },
            scope,
        });
        Ok(self)
    }
    pub fn tool_call_hook(
        &mut self,
        spec: HookSpec,
        hook: Arc<dyn ToolCallHook>,
    ) -> Result<&mut Self> {
        spec.validate()?;
        self.register(
            "tool_call_hook",
            &spec.id,
            Some(spec.priority),
            size(&spec, 1024)?,
        )?;
        self.call_hooks.push(Registered {
            owner: self.manifest.id.clone(),
            spec,
            callback: hook,
        });
        Ok(self)
    }
    pub fn tool_result_hook(
        &mut self,
        spec: HookSpec,
        hook: Arc<dyn ToolResultHook>,
    ) -> Result<&mut Self> {
        spec.validate()?;
        self.register(
            "tool_result_hook",
            &spec.id,
            Some(spec.priority),
            size(&spec, 1024)?,
        )?;
        self.result_hooks.push(Registered {
            owner: self.manifest.id.clone(),
            spec,
            callback: hook,
        });
        Ok(self)
    }
    /// Observation shares the existing bounded callback consumer and its drain timeout.
    pub fn observer(
        &mut self,
        id: impl Into<String>,
        priority: i32,
        callback: Arc<dyn AgentEventHandler>,
    ) -> Result<&mut Self> {
        let id = id.into();
        self.register("observer", &id, Some(priority), 0)?;
        self.observers.push(Observer { priority, callback });
        Ok(self)
    }
    pub fn command(
        &mut self,
        command: Arc<dyn ExtensionCommand>,
        timeout: std::time::Duration,
    ) -> Result<&mut Self> {
        let spec = command.specification();
        let mut execution = HookSpec::new(&spec.name);
        execution.timeout = timeout;
        name(&spec.name)?;
        ensure!(
            (std::time::Duration::from_millis(1)..=std::time::Duration::from_secs(600))
                .contains(&timeout),
            "extension command timeout must be 1 ms to 600 s"
        );
        ensure!(
            spec.parameters.is_object(),
            "extension command requires a JSON schema object"
        );
        self.register("command", &spec.name, None, size(&spec, 32 * 1024)?)?;
        self.commands.push(Command {
            owner: self.manifest.id.clone(),
            spec,
            execution,
            callback: command,
        });
        Ok(self)
    }
}
#[derive(Default)]
pub struct ExtensionRegistry {
    extensions: Vec<Extension>,
    reserved_commands: HashSet<String>,
}
impl ExtensionRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reserve_command(&mut self, value: impl Into<String>) -> Result<()> {
        let key = value.into();
        name(&key)?;
        ensure!(
            !self
                .extensions
                .iter()
                .flat_map(|e| &e.commands)
                .any(|c| c.spec.name == key),
            "command already installed"
        );
        ensure!(
            self.reserved_commands.len() < MAX_CONTRIBUTIONS,
            "reserved command budget exceeded"
        );
        self.reserved_commands.insert(key);
        Ok(())
    }
    /// Collision/budget checks complete before publishing any contribution.
    pub fn install(&mut self, extension: Extension) -> Result<()> {
        ensure!(
            self.extensions.len() < MAX_EXTENSIONS,
            "extension count budget exceeded"
        );
        ensure!(
            !self
                .extensions
                .iter()
                .any(|e| e.manifest.id == extension.manifest.id),
            "extension already installed"
        );
        let count = self
            .extensions
            .iter()
            .map(|e| e.registrations.len())
            .sum::<usize>();
        let bytes = self.extensions.iter().map(|e| e.bytes).sum::<usize>();
        ensure!(
            extension.registrations.len() <= MAX_CONTRIBUTIONS - count
                && extension.bytes <= MAX_METADATA_BYTES - bytes,
            "extension registry budget exceeded"
        );
        for item in &extension.registrations {
            if matches!(
                item.kind.as_str(),
                "tool" | "provider" | "context_policy" | "command"
            ) {
                ensure!(
                    !self
                        .extensions
                        .iter()
                        .flat_map(|e| &e.registrations)
                        .any(|old| old.kind == item.kind && old.name == item.name),
                    "extension contribution name collision: {}",
                    item.name
                );
            }
            ensure!(
                item.kind != "command" || !self.reserved_commands.contains(&item.name),
                "extension command collides with a host command"
            );
        }
        self.extensions.push(extension);
        Ok(())
    }
    pub fn finish(self) -> ExtensionSet {
        ExtensionSet {
            extensions: self.extensions,
        }
    }
}
/// Immutable host-selected snapshot. Replacing it never mutates agents already built from it.
#[derive(Default)]
pub struct ExtensionSet {
    extensions: Vec<Extension>,
}
impl ExtensionSet {
    /// Compose immutable native registrations in order; collisions publish no replacement.
    pub fn combine(&self, other: &Self) -> Result<Self> {
        let mut registry = ExtensionRegistry::new();
        for extension in self.extensions.iter().chain(&other.extensions) {
            registry.install(extension.clone())?;
        }
        Ok(registry.finish())
    }
    /// Read-only collision/budget validation against a frozen base tool catalog.
    pub fn validate_tools(&self, base: &ToolRegistry) -> Result<()> {
        self.registry(base).map(|_| ())
    }
    pub fn catalog(&self) -> ExtensionCatalog {
        ExtensionCatalog {
            extensions: self.extensions.iter().map(|e| e.manifest.clone()).collect(),
            registrations: self
                .extensions
                .iter()
                .flat_map(|e| e.registrations.clone())
                .collect(),
        }
    }
    pub fn provider(&self, route: &str) -> Option<Arc<dyn ModelProvider>> {
        self.extensions
            .iter()
            .flat_map(|e| &e.providers)
            .find(|(name, _)| name == route)
            .map(|(_, p)| p.clone())
    }
    pub fn context_policy(&self, name: &str) -> Option<Arc<dyn ContextPolicy>> {
        self.extensions
            .iter()
            .flat_map(|e| &e.policies)
            .find(|(key, _)| key == name)
            .map(|(_, p)| p.clone())
    }
    pub fn commands(&self) -> Vec<CommandSpec> {
        self.extensions
            .iter()
            .flat_map(|e| &e.commands)
            .map(|c| c.spec.clone())
            .collect()
    }
    pub async fn execute_command(
        &self,
        name: &str,
        request: CommandRequest<'_>,
        context: &TurnExecutionContext,
    ) -> Result<CommandOutput> {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let command = self
            .extensions
            .iter()
            .flat_map(|e| &e.commands)
            .find(|c| c.spec.name == name)
            .ok_or_else(|| anyhow::anyhow!("extension command not found"))?;
        size(request.arguments, 64 * 1024)?;
        let output = tokio::select! {biased;
            cancelled=context.cancelled()=>return Err(cancelled.into()),
            output=invoke(&command.owner,&command.execution,context.ui(),|cancellation|command.callback.execute(request,cancellation))=>output?,
        };
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        match &output {
            CommandOutput::Notice(text) => {
                size(text, 64 * 1024)?;
            }
            CommandOutput::Submit(input) => {
                // AgentInput already owns a validated user message; bound the borrowed
                // envelope before making any host copy of the command's output.
                size(input.message(), 2 * 1024 * 1024 - 16384)?;
            }
        }
        Ok(match output {
            CommandOutput::Submit(input) => {
                CommandOutput::Submit(input.with_source(crate::InputSource::Extension))
            }
            other => other,
        })
    }
    pub(crate) fn registry(&self, base: &ToolRegistry) -> Result<ToolRegistry> {
        base.with_additions(self.extensions.iter().flat_map(|e| e.tools.iter().cloned()))
    }
    pub(crate) fn input_context(
        &self,
        base: Option<Arc<dyn InputContextProvider>>,
    ) -> Arc<dyn InputContextProvider> {
        let mut hooks: Vec<_> = self
            .extensions
            .iter()
            .flat_map(|e| e.contexts.iter().cloned())
            .collect();
        hooks.sort_by_key(|h| h.spec.priority);
        Arc::new(ContextChain {
            base,
            hooks,
            manifests: self.extensions.iter().map(|e| e.manifest.clone()).collect(),
        })
    }
    pub(crate) fn tool_hooks(&self) -> Vec<Registered<dyn ToolHook>> {
        let mut hooks: Vec<_> = self
            .extensions
            .iter()
            .flat_map(|e| e.guards.iter().cloned())
            .collect();
        hooks.sort_by_key(|h| h.spec.priority);
        hooks
    }
    pub(crate) fn input_hooks(&self) -> Vec<Registered<dyn InputHook>> {
        let mut hooks = self
            .extensions
            .iter()
            .flat_map(|e| e.inputs.clone())
            .collect::<Vec<_>>();
        hooks.sort_by_key(|h| h.spec.priority);
        hooks
    }
    pub(crate) fn startup_hooks(&self) -> Vec<Registered<dyn BeforeStartHook>> {
        let mut hooks = self
            .extensions
            .iter()
            .flat_map(|e| e.startups.clone())
            .collect::<Vec<_>>();
        hooks.sort_by_key(|h| h.spec.priority);
        hooks
    }
    pub(crate) fn boundary_hooks(&self) -> Vec<Registered<dyn BoundaryHook>> {
        ordered(
            self.extensions
                .iter()
                .flat_map(|e| e.boundaries.iter().cloned()),
        )
    }
    pub(crate) fn request_hooks(&self) -> Vec<RequestRegistration> {
        let mut hooks = self
            .extensions
            .iter()
            .flat_map(|e| e.requests.iter().cloned())
            .collect::<Vec<_>>();
        hooks.sort_by_key(|r| r.hook.spec.priority);
        hooks
    }
    pub(crate) fn call_hooks(&self) -> Vec<Registered<dyn ToolCallHook>> {
        ordered(
            self.extensions
                .iter()
                .flat_map(|e| e.call_hooks.iter().cloned()),
        )
    }
    pub(crate) fn result_hooks(&self) -> Vec<Registered<dyn ToolResultHook>> {
        ordered(
            self.extensions
                .iter()
                .flat_map(|e| e.result_hooks.iter().cloned()),
        )
    }
    pub(crate) fn handler(&self, host: Arc<dyn AgentEventHandler>) -> Arc<dyn AgentEventHandler> {
        let mut observers: Vec<_> = self.extensions.iter().flat_map(|e| &e.observers).collect();
        if observers.is_empty() {
            return host;
        }
        observers.sort_by_key(|o| o.priority);
        Arc::new(Observation {
            host,
            observers: observers.into_iter().map(|o| o.callback.clone()).collect(),
        })
    }
}
struct Observation {
    host: Arc<dyn AgentEventHandler>,
    observers: Vec<Arc<dyn AgentEventHandler>>,
}
impl AgentEventHandler for Observation {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        crate::events::dispatch_event(self.host.as_ref(), event);
        for observer in &self.observers {
            crate::events::dispatch_event(observer.as_ref(), event);
        }
    }
}
struct PinnedTool {
    definition: ToolDefinition,
    capability: ToolCapability,
    inner: Arc<dyn ExecutableTool>,
}
#[async_trait]
impl ExecutableTool for PinnedTool {
    fn composition_factory(&self) -> Option<&dyn crate::composition::ToolCompositionFactory> {
        self.inner.composition_factory()
    }
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn capability(&self) -> ToolCapability {
        self.capability
    }
    async fn prepare(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        self.inner.prepare(context, arguments).await
    }
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        self.inner.execute(context, arguments).await
    }
}
