//! MCP stdio adapter over the bounded process transport.
mod config;
mod connection;
mod content;
pub use crate::process_extensions::mcp_peer::McpWireDiagnostics;
use crate::process_extensions::{
    ProcessDiagnostics, ProcessError, ProcessErrorKind, transport::Class,
};
use aicoder_core::{TurnExecutionContext, extensions::Extension, model::ToolDefinition, tools::*};
pub use config::{
    LEGACY_VERSIONS, MODERN_VERSION, McpConfig, McpLimits, McpProtocol, McpServerInfo,
};
pub use connection::McpToolInfo;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum McpErrorKind {
    Transport,
    Cancelled,
    Timeout,
    UnsupportedVersion,
    UnsupportedCapability,
    UnsupportedInteraction,
    IdentityMismatch,
    InvalidMetadata,
    InvalidCatalog,
    CatalogLimit,
    CatalogStale,
    InvalidResult,
    InvalidContent,
    Workspace,
}
#[derive(Debug, Clone)]
pub struct McpError {
    pub kind: McpErrorKind,
    pub process: Option<ProcessError>,
    pub supported_versions: Vec<String>,
}
impl McpError {
    fn new(kind: McpErrorKind) -> Self {
        Self {
            kind,
            process: None,
            supported_versions: Vec::new(),
        }
    }
    fn transport(error: ProcessError) -> Self {
        Self {
            kind: McpErrorKind::Transport,
            process: Some(error),
            supported_versions: Vec::new(),
        }
    }
}
impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MCP {:?}", self.kind)?;
        if let Some(e) = &self.process {
            write!(f, ": {e}")?;
        }
        Ok(())
    }
}
impl std::error::Error for McpError {}
struct Validation {
    expires: Instant,
    epoch: u64,
}
struct Snapshot {
    connection: Arc<connection::Connection>,
    tools: Vec<McpToolInfo>,
    validation: tokio::sync::Mutex<Validation>,
    invalid: AtomicBool,
}
impl Snapshot {
    async fn validate(&self, context: &ToolContext) -> Result<u64, McpError> {
        let connection = &self.connection;
        if !connection
            .client
            .shared
            .workspace_matches(context.workspace_root())
        {
            connection.client.shared.close(ProcessErrorKind::Workspace);
            return Err(McpError::new(McpErrorKind::Workspace));
        }
        if connection.client.shared.diagnostics().closed {
            return Err(McpError::transport(
                connection
                    .client
                    .shared
                    .error(ProcessErrorKind::Closed, false),
            ));
        }
        if self.invalid.load(Ordering::SeqCst) {
            return Err(McpError::new(McpErrorKind::CatalogStale));
        }
        let duration = context
            .tool_timeout()
            .min(connection.options.io.request_timeout);
        let work = async {
            let mut validation = self.validation.lock().await;
            if self.invalid.load(Ordering::SeqCst) {
                return Err(McpError::new(McpErrorKind::CatalogStale));
            }
            if connection.client.shared.diagnostics().closed {
                return Err(McpError::transport(
                    connection
                        .client
                        .shared
                        .error(ProcessErrorKind::Closed, false),
                ));
            }
            if Instant::now() < validation.expires
                && validation.epoch == connection.peer.epoch.load(Ordering::SeqCst)
            {
                return Ok(validation.epoch);
            }
            let fresh = connection.catalog(duration).await?;
            if self.invalid.load(Ordering::SeqCst) {
                return Err(McpError::new(McpErrorKind::CatalogStale));
            }
            if fresh.tools != self.tools {
                self.invalid.store(true, Ordering::SeqCst);
                return Err(McpError::new(McpErrorKind::CatalogStale));
            }
            validation.expires = Instant::now() + fresh.fresh_for;
            validation.epoch = fresh.epoch;
            Ok(validation.epoch)
        };
        tokio::time::timeout(duration, work)
            .await
            .map_err(|_| McpError::new(McpErrorKind::Timeout))?
    }
}
pub struct McpExtension {
    snapshot: Arc<Snapshot>,
}
impl std::fmt::Debug for McpExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpExtension")
            .field("version", &self.protocol_version())
            .field("server", self.server())
            .field("diagnostics", &self.diagnostics())
            .finish()
    }
}
impl McpExtension {
    pub async fn connect(
        config: McpConfig,
        context: TurnExecutionContext,
    ) -> Result<Self, McpError> {
        let connection = connection::connect(config, context.clone()).await?;
        let result = Self::discover(connection.clone(), context, true).await;
        if result.is_err() {
            connection.client.shutdown().await;
        }
        result
    }
    async fn discover(
        connection: Arc<connection::Connection>,
        context: TurnExecutionContext,
        close_on_error: bool,
    ) -> Result<Self, McpError> {
        struct Guard(Arc<connection::Connection>, bool);
        impl Drop for Guard {
            fn drop(&mut self) {
                if self.1 {
                    self.0.client.shared.close(ProcessErrorKind::Cancelled);
                }
            }
        }
        let mut guard = Guard(connection.clone(), close_on_error);
        let timeout = connection.options.io.initialize_timeout;
        let catalog = tokio::select! {biased;
            _=context.cancelled()=>return Err(McpError::new(McpErrorKind::Cancelled)),
            result=tokio::time::timeout(timeout,connection.catalog(timeout))=>result.map_err(|_|McpError::new(McpErrorKind::Timeout))??,
        };
        let result = Self {
            snapshot: Arc::new(Snapshot {
                connection,
                tools: catalog.tools,
                validation: tokio::sync::Mutex::new(Validation {
                    expires: Instant::now() + catalog.fresh_for,
                    epoch: catalog.epoch,
                }),
                invalid: AtomicBool::new(false),
            }),
        };
        result
            .extension()
            .map_err(|_| McpError::new(McpErrorKind::InvalidCatalog))?;
        guard.1 = false;
        Ok(result)
    }
    pub async fn refresh(&self, context: TurnExecutionContext) -> Result<Self, McpError> {
        Self::discover(self.snapshot.connection.clone(), context, false).await
    }
    pub fn protocol_version(&self) -> &str {
        &self.snapshot.connection.version
    }
    pub fn server(&self) -> &McpServerInfo {
        &self.snapshot.connection.server
    }
    pub fn instructions(&self) -> Option<&str> {
        self.snapshot.connection.instructions.as_deref()
    }
    pub fn server_capabilities(&self) -> &Value {
        &self.snapshot.connection.capabilities
    }
    pub fn catalog(&self) -> &[McpToolInfo] {
        &self.snapshot.tools
    }
    pub fn diagnostics(&self) -> ProcessDiagnostics {
        self.snapshot.connection.client.shared.diagnostics()
    }
    pub fn wire_diagnostics(&self) -> McpWireDiagnostics {
        self.snapshot.connection.peer.diagnostics()
    }
    pub async fn shutdown(&self) -> ProcessDiagnostics {
        self.snapshot.connection.client.shutdown().await
    }
    pub fn tools(&self) -> Vec<Arc<dyn ExecutableTool>> {
        (0..self.snapshot.tools.len())
            .map(|index| {
                Arc::new(McpTool {
                    snapshot: self.snapshot.clone(),
                    index,
                }) as Arc<dyn ExecutableTool>
            })
            .collect()
    }
    pub fn extension(&self) -> anyhow::Result<Extension> {
        let version = format!(
            "mcp:{}:{:016x}",
            self.protocol_version(),
            config::fingerprint(&(self.server(), &self.snapshot.tools))
        );
        let mut extension = Extension::new(&self.snapshot.connection.options.namespace, version)?;
        for tool in self.tools() {
            extension.tool(tool)?;
        }
        Ok(extension)
    }
}
struct McpTool {
    snapshot: Arc<Snapshot>,
    index: usize,
}
fn failure(error: McpError) -> ToolFailure {
    ToolFailure::new(match error.kind {McpErrorKind::CatalogStale=>"mcp_catalog_stale",McpErrorKind::UnsupportedInteraction=>"mcp_interaction_unavailable",_=>"mcp_failed"},error.to_string()).with_details(json!({"kind":error.kind,"may_have_executed":error.process.as_ref().is_some_and(|e|e.may_have_executed),"remote_code":error.process.as_ref().and_then(|e|e.remote_code)}))
}
#[async_trait::async_trait]
impl ExecutableTool for McpTool {
    fn definition(&self) -> ToolDefinition {
        self.snapshot.tools[self.index].definition()
    }
    fn capability(&self) -> ToolCapability {
        self.snapshot.tools[self.index].capability
    }
    async fn prepare(
        &self,
        context: &ToolContext,
        _: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        self.snapshot.validate(context).await.map_err(failure)?;
        Ok(None)
    }
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        if !arguments.is_object() {
            return Err(ToolFailure::new(
                "invalid_arguments",
                "MCP arguments must be an object",
            ));
        }
        let epoch = self.snapshot.validate(context).await.map_err(failure)?;
        let connection = &self.snapshot.connection;
        let params = connection
            .params(json!({"name":self.snapshot.tools[self.index].name,"arguments":arguments}));
        let result = connection
            .client
            .call_at_epoch(
                "tools/call",
                params,
                Class::Execute,
                context
                    .tool_timeout()
                    .min(connection.options.io.request_timeout),
                epoch,
            )
            .await
            .map_err(|e| {
                if e.kind == ProcessErrorKind::InvalidCatalog {
                    self.snapshot.invalid.store(true, Ordering::SeqCst);
                    failure(McpError::new(McpErrorKind::CatalogStale))
                } else {
                    failure(McpError::transport(e))
                }
            })?;
        let result = connection.complete(&result).map_err(|error| {
            let mut failure=failure(error);
            failure.message="MCP call returned an incomplete or unsupported result; inspect state before repeating the operation".into();
            failure.details=Some(json!({"rpc_reply_received":true,"operation_completed":false,"may_have_executed":true,"delivery_complete":false}));failure
        })?;
        content::normalize(result, context, &connection.options.limits).await
    }
}
