use super::{
    transport::{Class, Client, Shared},
    *,
};
use aicoder_core::{
    TurnExecutionContext,
    extensions::{Extension, process_protocol::*},
    model::ToolDefinition,
    tools::{ExecutableTool, PreparedTool, ToolContext, ToolFailure, ToolPreview, ToolSuccess},
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
};

pub struct ProcessExtension {
    client: Arc<Client>,
    server: ServerInfo,
    namespace: String,
    tools: Vec<Arc<dyn ExecutableTool>>,
}
impl std::fmt::Debug for ProcessExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessExtension")
            .field("namespace", &self.namespace)
            .field("server", &self.server)
            .field("diagnostics", &self.diagnostics())
            .finish()
    }
}
impl ProcessExtension {
    pub async fn connect(
        config: ProcessConfig,
        context: TurnExecutionContext,
    ) -> Result<Self, ProcessError> {
        if context.is_cancelled() {
            return Err(ProcessError {
                kind: ProcessErrorKind::Cancelled,
                may_have_executed: false,
                remote_code: None,
                diagnostics: Default::default(),
            });
        }
        if !config.valid() {
            return Err(ProcessError {
                kind: ProcessErrorKind::InvalidConfiguration,
                may_have_executed: false,
                remote_code: None,
                diagnostics: Default::default(),
            });
        }
        let namespace = config.namespace.clone();
        let aliases = config.aliases.clone();
        let capabilities = config.capabilities.clone();
        let expected = config.expected_server.clone();
        let limits = config.limits;
        let client = Client::start(config)?;
        let result = tokio::select! {biased;
            _=context.cancelled()=>Err(client.shared.error(ProcessErrorKind::Cancelled,false)),
            result=client.call(INITIALIZE,Initialize {protocol_version:PROTOCOL_VERSION.into()},Class::Initialize,limits.initialize_timeout)=>result,
        };
        let result = result.and_then(|value| {
            Self::catalog(
                client.clone(),
                namespace,
                aliases,
                capabilities,
                expected,
                limits,
                value,
            )
        });
        if result.is_err() || context.is_cancelled() {
            client.shared.close(if context.is_cancelled() {
                ProcessErrorKind::Cancelled
            } else {
                result
                    .as_ref()
                    .err()
                    .map_or(ProcessErrorKind::InvalidCatalog, |e| e.kind)
            });
            client.shutdown().await;
            return Err(if context.is_cancelled() {
                client.shared.error(ProcessErrorKind::Cancelled, false)
            } else {
                let mut error = result.expect_err("failed initialization");
                error.diagnostics = client.shared.diagnostics();
                error
            });
        }
        result
    }
    fn catalog(
        client: Arc<Client>,
        namespace: String,
        aliases: BTreeMap<String, String>,
        capabilities: BTreeMap<String, ToolCapability>,
        expected: Option<ServerInfo>,
        limits: ProcessLimits,
        value: Value,
    ) -> Result<Self, ProcessError> {
        let invalid = || client.shared.error(ProcessErrorKind::InvalidCatalog, false);
        let catalog: Initialized = serde_json::from_value(value).map_err(|_| invalid())?;
        if catalog.protocol_version != PROTOCOL_VERSION
            || !valid_name(&catalog.server_info.id)
            || catalog.server_info.version.is_empty()
            || catalog.server_info.version.len() > 128
            || !catalog
                .server_info
                .version
                .bytes()
                .all(|b| b.is_ascii_graphic())
            || expected.is_some_and(|s| s != catalog.server_info)
            || catalog.tools.len() > limits.max_tools
            || !super::transport::fits(&catalog, 1024 * 1024)
        {
            return Err(invalid());
        }
        let mut names = HashSet::new();
        let mut mapped = HashSet::new();
        let mut tools: Vec<Arc<dyn ExecutableTool>> = Vec::new();
        for remote in catalog.tools {
            let mut definition = remote.definition;
            let original = definition.name.clone();
            if !valid_name(&original)
                || !definition.parameters.is_object()
                || !names.insert(original.clone())
                || !super::transport::fits(&definition, 64 * 1024)
            {
                return Err(invalid());
            }
            definition.name = aliases
                .get(&original)
                .cloned()
                .unwrap_or_else(|| format!("{namespace}__{original}"));
            if !valid_name(&definition.name) || !mapped.insert(definition.name.clone()) {
                return Err(invalid());
            }
            let capability = capabilities
                .get(&original)
                .copied()
                .unwrap_or(ToolCapability::Mutating);
            tools.push(Arc::new(ProcessTool {
                client: client.clone(),
                definition,
                remote: original,
                preparation: remote.preparation,
                capability,
            }));
        }
        if aliases
            .keys()
            .chain(capabilities.keys())
            .any(|name| !names.contains(name))
        {
            return Err(invalid());
        }
        let extension = Self {
            client: client.clone(),
            server: catalog.server_info,
            namespace,
            tools,
        };
        extension.extension().map_err(|_| invalid())?;
        Ok(extension)
    }
    pub fn server(&self) -> &ServerInfo {
        &self.server
    }
    pub fn diagnostics(&self) -> ProcessDiagnostics {
        self.client.shared.diagnostics()
    }
    pub fn tools(&self) -> Vec<Arc<dyn ExecutableTool>> {
        self.tools.clone()
    }
    /// Each installed native snapshot receives normal fresh registry authority.
    pub fn extension(&self) -> anyhow::Result<Extension> {
        let mut extension = Extension::new(&self.namespace, &self.server.version)?;
        for tool in &self.tools {
            extension.tool(tool.clone())?;
        }
        Ok(extension)
    }
    /// Synchronous invalidation; the OS worker owns bounded cleanup independently of Tokio.
    pub(crate) fn abort(&self) {
        self.client.shared.abort();
    }
    pub fn request_shutdown(&self) {
        self.client.shared.close(super::ProcessErrorKind::Shutdown);
    }
    pub async fn shutdown(&self) -> ProcessDiagnostics {
        self.client.shutdown().await
    }
}
struct ProcessTool {
    client: Arc<Client>,
    definition: ToolDefinition,
    remote: String,
    preparation: bool,
    capability: ToolCapability,
}
fn check(shared: &Shared, context: &ToolContext) -> Result<(), ToolFailure> {
    if !shared.workspace_matches(context.workspace_root()) {
        shared.close(ProcessErrorKind::Workspace);
        return Err(failure(shared.error(ProcessErrorKind::Workspace, false)));
    }
    if context.tool_timeout().is_zero() {
        return Err(failure(shared.error(ProcessErrorKind::Timeout, false)));
    }
    Ok(())
}
fn failure(error: ProcessError) -> ToolFailure {
    ToolFailure::new(match error.kind {ProcessErrorKind::Busy=>"process_busy",ProcessErrorKind::Workspace=>"process_workspace",ProcessErrorKind::Remote=>"process_tool_error",ProcessErrorKind::PlanLimit=>"process_plan_limit",_=>"process_extension_failed"},error.to_string())
        .with_details(json!({"kind":error.kind,"may_have_executed":error.may_have_executed,"remote_code":error.remote_code,"exit_code":error.diagnostics.exit_code,"signal":error.diagnostics.signal,"cleanup_pending":error.diagnostics.cleanup_pending}))
}
struct Slot(Arc<Shared>);
impl Slot {
    fn reserve(shared: Arc<Shared>) -> Result<Self, ToolFailure> {
        shared
            .plans
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < shared.limits.max_plans).then_some(n + 1)
            })
            .map_err(|_| failure(shared.error(ProcessErrorKind::PlanLimit, false)))?;
        Ok(Self(shared))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.plans.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl ExecutableTool for ProcessTool {
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
        check(&self.client.shared, context)?;
        if !self.preparation {
            return Ok(None);
        }
        let slot = Slot::reserve(self.client.shared.clone())?;
        let value = self
            .client
            .call(
                PREPARE,
                Invocation {
                    tool: self.remote.clone(),
                    arguments,
                },
                Class::Prepare,
                context
                    .tool_timeout()
                    .min(self.client.shared.limits.request_timeout),
            )
            .await
            .map_err(failure)?;
        let prepared = serde_json::from_value::<Prepared>(value).ok().filter(|p| {
            !p.token.is_empty()
                && super::transport::fits(&p.token, 4096)
                && super::transport::fits(&p.preview, aicoder_core::tools::MAX_TOOL_PREVIEW_BYTES)
        });
        let Some(prepared) = prepared else {
            self.client.shared.close(ProcessErrorKind::InvalidResult);
            return Err(failure(
                self.client
                    .shared
                    .error(ProcessErrorKind::InvalidResult, false),
            ));
        };
        Ok(Some(Box::new(RemotePlan {
            client: self.client.clone(),
            remote: self.remote.clone(),
            token: Some(prepared.token),
            preview: prepared.preview,
            _slot: slot,
        })))
    }
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        check(&self.client.shared, context)?;
        if self.preparation {
            return Err(ToolFailure::new(
                "prepared_operation_required",
                "This tool requires an owned approved plan",
            ));
        }
        let result = self
            .client
            .call(
                CALL,
                Invocation {
                    tool: self.remote.clone(),
                    arguments,
                },
                Class::Execute,
                context
                    .tool_timeout()
                    .min(self.client.shared.limits.request_timeout),
            )
            .await
            .map_err(failure)?;
        completed(&self.client, result)
    }
}
fn completed(client: &Client, value: Value) -> Result<ToolSuccess, ToolFailure> {
    let output: Completed = serde_json::from_value(value).map_err(|_| {
        client.shared.close(ProcessErrorKind::InvalidResult);
        failure(client.shared.error(ProcessErrorKind::InvalidResult, true))
    })?;
    Ok(ToolSuccess {
        output: output.output,
        truncated: output.truncated,
        artifacts: Vec::new(),
    })
}
struct RemotePlan {
    client: Arc<Client>,
    remote: String,
    token: Option<String>,
    preview: ToolPreview,
    _slot: Slot,
}
impl Drop for RemotePlan {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.client.dispose(Plan {
                tool: self.remote.clone(),
                token,
            });
        }
    }
}
#[async_trait]
impl PreparedTool for RemotePlan {
    fn preview(&self) -> ToolPreview {
        self.preview.clone()
    }
    async fn execute(
        mut self: Box<Self>,
        context: &ToolContext,
    ) -> Result<ToolSuccess, ToolFailure> {
        check(&self.client.shared, context)?;
        let result = self
            .client
            .call(
                COMMIT,
                Plan {
                    tool: self.remote.clone(),
                    token: self.token.as_ref().expect("owned plan").clone(),
                },
                Class::Execute,
                context
                    .tool_timeout()
                    .min(self.client.shared.limits.request_timeout),
            )
            .await
            .map_err(failure)?;
        self.token = None;
        completed(&self.client, result)
    }
}
