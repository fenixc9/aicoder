use super::{config::*, *};
use crate::process_extensions::{
    ProcessErrorKind,
    mcp_peer::{LEGACY, MODERN, McpPeer},
    transport::{Class, Client},
};
use aicoder_core::{TurnExecutionContext, model::ToolDefinition, tools::ToolCapability};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpToolInfo {
    pub name: String,
    pub model_name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: Option<Value>,
    pub execution: Option<Value>,
    pub capability: ToolCapability,
}
impl McpToolInfo {
    pub(crate) fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.model_name.clone(),
            description: self.description.clone(),
            parameters: self.input_schema.clone(),
        }
    }
}
pub(crate) struct Options {
    pub namespace: String,
    pub aliases: std::collections::BTreeMap<String, String>,
    pub capabilities: std::collections::BTreeMap<String, ToolCapability>,
    pub limits: McpLimits,
    pub io: crate::process_extensions::ProcessLimits,
}
pub(crate) struct Connection {
    pub client: Arc<Client>,
    pub peer: Arc<McpPeer>,
    pub options: Options,
    pub version: String,
    pub server: McpServerInfo,
    pub instructions: Option<String>,
    pub capabilities: Value,
}
pub(crate) struct Catalog {
    pub tools: Vec<McpToolInfo>,
    pub fresh_for: Duration,
    pub epoch: u64,
}
impl Connection {
    pub fn modern(&self) -> bool {
        self.version == MODERN_VERSION
    }
    pub fn params(&self, mut fields: Value) -> Value {
        if self.modern() {
            fields
                .as_object_mut()
                .expect("MCP params object")
                .insert("_meta".into(), meta(&self.version));
        }
        fields
    }
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        class: Class,
        duration: Duration,
    ) -> Result<Value, McpError> {
        self.client
            .call(method, self.params(params), class, duration)
            .await
            .map_err(McpError::transport)
    }
    pub fn complete<'a>(&self, value: &'a Value) -> Result<&'a Value, McpError> {
        if !value.is_object() {
            return Err(McpError::new(McpErrorKind::InvalidResult));
        }
        match value.get("resultType").and_then(Value::as_str) {
            Some("complete") => Ok(value),
            None if !self.modern() => Ok(value),
            Some("input_required") => Err(McpError::new(McpErrorKind::UnsupportedInteraction)),
            _ => Err(McpError::new(McpErrorKind::InvalidResult)),
        }
    }
    pub fn freshness(&self, value: &Value) -> Result<Duration, McpError> {
        if !self.modern() {
            return Ok(self.options.limits.max_catalog_age);
        }
        if !matches!(
            value.get("cacheScope").and_then(Value::as_str),
            Some("public" | "private")
        ) {
            return Err(McpError::new(McpErrorKind::InvalidMetadata));
        }
        let ms = value
            .get("ttlMs")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or_else(|| McpError::new(McpErrorKind::InvalidMetadata))?;
        Ok(Duration::from_secs_f64(
            (ms / 1000.0).min(self.options.limits.max_catalog_age.as_secs_f64()),
        ))
    }
    pub async fn catalog(&self, duration: Duration) -> Result<Catalog, McpError> {
        let start = self.peer.epoch.load(Ordering::SeqCst);
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        let mut names = HashSet::new();
        let mut aliases = HashSet::new();
        let mut tools = Vec::new();
        let mut bytes = 0usize;
        let mut freshness = self.options.limits.max_catalog_age;
        for _ in 0..self.options.limits.max_pages {
            let mut params = json!({});
            if let Some(cursor) = &cursor {
                params["cursor"] = Value::String(cursor.clone());
            }
            let value = self
                .request("tools/list", params, Class::Initialize, duration)
                .await?;
            let value = self.complete(&value)?;
            freshness = freshness.min(self.freshness(value)?);
            let page = value
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| McpError::new(McpErrorKind::InvalidCatalog))?;
            for tool in page {
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| remote_name(n))
                    .ok_or_else(|| McpError::new(McpErrorKind::InvalidCatalog))?;
                if !names.insert(name.to_owned())
                    || tools.len() >= self.options.io.max_tools
                    || !crate::process_extensions::transport::fits(tool, 64 * 1024)
                {
                    return Err(McpError::new(McpErrorKind::InvalidCatalog));
                }
                let input_schema = tool
                    .get("inputSchema")
                    .filter(|s| s.is_object())
                    .ok_or_else(|| McpError::new(McpErrorKind::InvalidCatalog))?
                    .clone();
                if input_schema.get("type").is_some_and(|v| v != "object") {
                    return Err(McpError::new(McpErrorKind::InvalidCatalog));
                }
                let model_name = self
                    .options
                    .aliases
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| alias(&self.options.namespace, name));
                if !aliases.insert(model_name.clone()) {
                    return Err(McpError::new(McpErrorKind::InvalidCatalog));
                }
                if tool
                    .get("execution")
                    .and_then(|v| v.get("taskSupport"))
                    .is_some_and(|v| v == "required")
                {
                    return Err(McpError::new(McpErrorKind::UnsupportedCapability));
                }
                let string = |key: &str| -> Result<Option<String>, McpError> {
                    match tool.get(key) {
                        None => Ok(None),
                        Some(Value::String(s)) => Ok(Some(s.clone())),
                        _ => Err(McpError::new(McpErrorKind::InvalidCatalog)),
                    }
                };
                let info = McpToolInfo {
                    name: name.into(),
                    model_name,
                    title: string("title")?,
                    description: string("description")?,
                    input_schema,
                    output_schema: tool.get("outputSchema").cloned(),
                    annotations: tool.get("annotations").cloned(),
                    execution: tool.get("execution").cloned(),
                    capability: self
                        .options
                        .capabilities
                        .get(name)
                        .copied()
                        .unwrap_or(ToolCapability::Mutating),
                };
                if info.output_schema.as_ref().is_some_and(|s| !s.is_object()) {
                    return Err(McpError::new(McpErrorKind::InvalidCatalog));
                }
                bytes = bytes.saturating_add(
                    serde_json::to_vec(&info)
                        .map_err(|_| McpError::new(McpErrorKind::InvalidCatalog))?
                        .len(),
                );
                if bytes > 1024 * 1024 {
                    return Err(McpError::new(McpErrorKind::InvalidCatalog));
                }
                tools.push(info);
            }
            match value.get("nextCursor") {
                None => {
                    if self.peer.epoch.load(Ordering::SeqCst) != start {
                        return Err(McpError::new(McpErrorKind::CatalogStale));
                    }
                    if self
                        .options
                        .aliases
                        .keys()
                        .chain(self.options.capabilities.keys())
                        .any(|n| !names.contains(n))
                    {
                        return Err(McpError::new(McpErrorKind::InvalidCatalog));
                    }
                    tools.sort_by(|a, b| a.name.cmp(&b.name));
                    return Ok(Catalog {
                        tools,
                        fresh_for: freshness,
                        epoch: start,
                    });
                }
                Some(Value::String(next)) if next.len() <= 4096 && cursors.insert(next.clone()) => {
                    cursor = Some(next.clone())
                }
                _ => return Err(McpError::new(McpErrorKind::InvalidCatalog)),
            }
        }
        Err(McpError::new(McpErrorKind::CatalogLimit))
    }
}
pub(crate) fn meta(version: &str) -> Value {
    json!({"io.modelcontextprotocol/protocolVersion":version,"io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"aicoder","version":env!("CARGO_PKG_VERSION")}})
}
fn server(value: Option<&Value>, fallback: &str) -> Result<McpServerInfo, McpError> {
    match value {
        None => Ok(McpServerInfo {
            name: fallback.into(),
            version: "unspecified".into(),
        }),
        Some(value) => {
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control));
            let version = value
                .get("version")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control));
            Ok(McpServerInfo {
                name: name
                    .ok_or_else(|| McpError::new(McpErrorKind::InvalidMetadata))?
                    .into(),
                version: version
                    .ok_or_else(|| McpError::new(McpErrorKind::InvalidMetadata))?
                    .into(),
            })
        }
    }
}
struct ConnectGuard {
    client: Arc<Client>,
    armed: bool,
}
impl Drop for ConnectGuard {
    fn drop(&mut self) {
        if self.armed {
            self.client.shared.close(ProcessErrorKind::Cancelled);
        }
    }
}
pub(crate) async fn connect(
    config: McpConfig,
    context: TurnExecutionContext,
) -> Result<Arc<Connection>, McpError> {
    config.validate().map_err(McpError::transport)?;
    if context.is_cancelled() {
        return Err(McpError::new(McpErrorKind::Cancelled));
    }
    let options = Options {
        namespace: config.process.namespace.clone(),
        aliases: config.aliases,
        capabilities: config.capabilities,
        limits: config.limits,
        io: config.process.limits,
    };
    let peer = Arc::new(McpPeer::new(
        options.limits.max_aux_messages_per_second,
        options.limits.max_aux_bytes_per_second,
    ));
    let client = Client::start_mcp(config.process, peer.clone()).map_err(McpError::transport)?;
    let mut guard = ConnectGuard {
        client: client.clone(),
        armed: true,
    };
    let timeout = options.io.initialize_timeout;
    let work = async {
        let mut modern = !matches!(config.protocol, McpProtocol::Legacy { .. });
        let mut discovered = None;
        if modern {
            if matches!(config.protocol, McpProtocol::Modern) {
                peer.era.store(MODERN, Ordering::SeqCst);
            }
            match client
                .call(
                    "server/discover",
                    json!({"_meta":meta(MODERN_VERSION)}),
                    Class::Probe,
                    options.limits.probe_timeout,
                )
                .await
            {
                Ok(value) => {
                    if value.get("resultType").and_then(Value::as_str) != Some("complete")
                        || !value
                            .get("supportedVersions")
                            .and_then(Value::as_array)
                            .is_some_and(|versions| versions.iter().any(|v| v == MODERN_VERSION))
                    {
                        return Err(McpError::new(McpErrorKind::UnsupportedVersion));
                    }
                    peer.era.store(MODERN, Ordering::SeqCst);
                    discovered = Some(value);
                }
                Err(error) => {
                    let recognized = error
                        .remote_code
                        .is_some_and(|c| matches!(c, -32022..=-32020));
                    if recognized || matches!(config.protocol, McpProtocol::Modern) {
                        let mut failure = McpError::new(if error.remote_code == Some(-32021) {
                            McpErrorKind::UnsupportedCapability
                        } else {
                            McpErrorKind::UnsupportedVersion
                        });
                        failure.supported_versions = peer.supported_versions();
                        return Err(failure);
                    }
                    if !(error.kind == ProcessErrorKind::Remote
                        || error.kind == ProcessErrorKind::Timeout)
                        || error.diagnostics.closed
                    {
                        return Err(McpError::transport(error));
                    }
                    modern = false;
                }
            }
        }
        let (version, value) = if modern {
            (
                MODERN_VERSION.to_owned(),
                discovered.expect("modern discovery"),
            )
        } else {
            peer.era.store(LEGACY, Ordering::SeqCst);
            let requested = match &config.protocol {
                McpProtocol::Legacy { version } => version.as_str(),
                _ => LEGACY_VERSIONS[0],
            };
            let value=client.call("initialize",json!({"protocolVersion":requested,"capabilities":{},"clientInfo":{"name":"aicoder","version":env!("CARGO_PKG_VERSION")}}),Class::Initialize,timeout).await.map_err(McpError::transport)?;
            let version = value
                .get("protocolVersion")
                .and_then(Value::as_str)
                .filter(|v| LEGACY_VERSIONS.contains(v))
                .ok_or_else(|| McpError::new(McpErrorKind::UnsupportedVersion))?
                .to_owned();
            client
                .call(
                    "notifications/initialized",
                    json!({}),
                    Class::Notification,
                    timeout,
                )
                .await
                .map_err(McpError::transport)?;
            (version, value)
        };
        let capabilities = value
            .get("capabilities")
            .filter(|v| v.is_object())
            .ok_or_else(|| McpError::new(McpErrorKind::InvalidMetadata))?
            .clone();
        if !capabilities.get("tools").is_some_and(Value::is_object) {
            return Err(McpError::new(McpErrorKind::UnsupportedCapability));
        }
        let info = server(
            if modern {
                value
                    .get("_meta")
                    .and_then(|m| m.get("io.modelcontextprotocol/serverInfo"))
            } else {
                value.get("serverInfo")
            },
            &options.namespace,
        )?;
        if config
            .expected_server
            .as_ref()
            .is_some_and(|expected| expected != &info)
        {
            return Err(McpError::new(McpErrorKind::IdentityMismatch));
        }
        let instructions = match value.get("instructions") {
            None => None,
            Some(Value::String(s)) if s.len() <= 16 * 1024 => Some(s.clone()),
            _ => return Err(McpError::new(McpErrorKind::InvalidMetadata)),
        };
        let connection = Arc::new(Connection {
            client: client.clone(),
            peer,
            options,
            version,
            server: info,
            instructions,
            capabilities,
        });
        if connection.modern() {
            connection.freshness(&value)?;
        }
        Ok(connection)
    };
    let result = tokio::select! {biased;
        _=context.cancelled()=>Err(McpError::new(McpErrorKind::Cancelled)),
        result=tokio::time::timeout(timeout,work)=>result.unwrap_or_else(|_|Err(McpError::new(McpErrorKind::Timeout))),
    };
    match result {
        Ok(connection) => {
            guard.armed = false;
            Ok(connection)
        }
        Err(error) => {
            client.shared.close(ProcessErrorKind::Cancelled);
            client.shutdown().await;
            guard.armed = false;
            Err(error)
        }
    }
}
