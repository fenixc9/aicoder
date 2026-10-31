use crate::process_extensions::{ProcessConfig, ProcessError, ProcessErrorKind};
use aicoder_core::tools::ToolCapability;
use serde::Serialize;
use std::{collections::BTreeMap, time::Duration};
pub const MODERN_VERSION: &str = "2026-07-28";
pub const LEGACY_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpProtocol {
    Auto,
    Modern,
    Legacy { version: String },
}
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct McpLimits {
    pub max_pages: usize,
    pub max_content_blocks: usize,
    pub max_media_bytes: usize,
    pub max_aux_messages_per_second: usize,
    pub max_aux_bytes_per_second: usize,
    pub max_catalog_age: Duration,
    pub probe_timeout: Duration,
}
impl Default for McpLimits {
    fn default() -> Self {
        Self {
            max_pages: 16,
            max_content_blocks: 64,
            max_media_bytes: 512 * 1024,
            max_aux_messages_per_second: 128,
            max_aux_bytes_per_second: 1024 * 1024,
            max_catalog_age: Duration::from_secs(300),
            probe_timeout: Duration::from_millis(500),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpServerInfo {
    pub name: String,
    pub version: String,
}
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct McpConfig {
    pub process: ProcessConfig,
    pub protocol: McpProtocol,
    pub aliases: BTreeMap<String, String>,
    pub capabilities: BTreeMap<String, ToolCapability>,
    pub expected_server: Option<McpServerInfo>,
    pub limits: McpLimits,
}
impl McpConfig {
    pub fn new(process: ProcessConfig) -> Self {
        Self {
            process,
            protocol: McpProtocol::Auto,
            aliases: BTreeMap::new(),
            capabilities: BTreeMap::new(),
            expected_server: None,
            limits: Default::default(),
        }
    }
    pub(crate) fn validate(&self) -> Result<(), ProcessError> {
        let valid = self.process.valid()
            && self.process.aliases.is_empty()
            && self.process.capabilities.is_empty()
            && self.process.expected_server.is_none()
            && self.aliases.len() <= 128
            && self.capabilities.len() <= 128
            && self
                .aliases
                .iter()
                .all(|(k, v)| remote_name(k) && crate::process_extensions::valid_name(v))
            && self.capabilities.keys().all(|k| remote_name(k))
            && (1..=64).contains(&self.limits.max_pages)
            && (1..=256).contains(&self.limits.max_content_blocks)
            && (1..=1024 * 1024).contains(&self.limits.max_media_bytes)
            && (1..=1024).contains(&self.limits.max_aux_messages_per_second)
            && (1024..=16 * 1024 * 1024).contains(&self.limits.max_aux_bytes_per_second)
            && (Duration::from_millis(1)..=Duration::from_secs(3600))
                .contains(&self.limits.max_catalog_age)
            && (Duration::from_millis(1)..=Duration::from_secs(30))
                .contains(&self.limits.probe_timeout)
            && self.expected_server.as_ref().is_none_or(|info| {
                !info.name.is_empty()
                    && info.name.len() <= 256
                    && !info.name.chars().any(char::is_control)
                    && !info.version.is_empty()
                    && info.version.len() <= 128
                    && !info.version.chars().any(char::is_control)
            })
            && match &self.protocol {
                McpProtocol::Legacy { version } => LEGACY_VERSIONS.contains(&version.as_str()),
                _ => true,
            };
        if !valid {
            return Err(ProcessError {
                kind: ProcessErrorKind::InvalidConfiguration,
                may_have_executed: false,
                remote_code: None,
                diagnostics: Default::default(),
            });
        }
        Ok(())
    }
}
pub(crate) fn remote_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 1024
        && name.chars().count() <= 128
        && !name.chars().any(char::is_control)
}
pub(crate) fn fingerprint(value: &impl Serialize) -> u64 {
    let bytes = serde_json::to_vec(value).expect("bounded MCP metadata serialization");
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}
pub(crate) fn alias(namespace: &str, remote: &str) -> String {
    let available = 64 - namespace.len() - 2;
    if crate::process_extensions::valid_name(remote) && remote.len() <= available {
        return format!("{namespace}__{remote}");
    }
    let readable: String = remote
        .bytes()
        .filter(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
        .take(available - 17)
        .map(char::from)
        .collect();
    format!("{namespace}__{readable}_{:016x}", fingerprint(&remote))
}
