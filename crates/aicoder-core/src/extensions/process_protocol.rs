//! Serializable process-tool contracts. Concrete transport/process IO stays in runtime.
use crate::{model::ToolDefinition, tools::ToolPreview};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub const PROTOCOL_VERSION: &str = "aicoder.tools.v1";
pub const INITIALIZE: &str = "initialize";
pub const PREPARE: &str = "tools/prepare";
pub const CALL: &str = "tools/call";
pub const COMMIT: &str = "tools/commit";
pub const DISPOSE: &str = "tools/dispose";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Initialize {
    pub protocol_version: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServerInfo {
    pub id: String,
    pub version: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteTool {
    pub definition: ToolDefinition,
    pub preparation: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Initialized {
    pub protocol_version: String,
    pub server_info: ServerInfo,
    pub tools: Vec<RemoteTool>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    pub tool: String,
    pub arguments: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub tool: String,
    pub token: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepared {
    pub token: String,
    pub preview: ToolPreview,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completed {
    pub output: Value,
    #[serde(default)]
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disposed {
    pub disposed: bool,
}
