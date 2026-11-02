//! Explicit bounded stdio tool processes. Programs are trusted host code, not sandboxes.
#[cfg(feature = "mcp-stdio")]
pub(crate) mod mcp_peer;
mod tools;
pub(crate) mod transport;
use aicoder_core::{extensions::process_protocol::ServerInfo, tools::ToolCapability};
use serde::Serialize;
use std::{collections::BTreeMap, fmt, path::PathBuf, time::Duration};
pub use tools::ProcessExtension;

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ProcessLimits {
    pub max_request_bytes: usize,
    pub max_frame_bytes: usize,
    pub max_queued: usize,
    pub max_plans: usize,
    pub max_tools: usize,
    pub max_stderr_bytes: u64,
    pub initialize_timeout: Duration,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
}
impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 256 * 1024,
            max_frame_bytes: 1024 * 1024,
            max_queued: 16,
            max_plans: 64,
            max_tools: 64,
            max_stderr_bytes: 16 * 1024 * 1024,
            initialize_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(1),
        }
    }
}
impl ProcessLimits {
    pub(crate) fn valid(&self) -> bool {
        (1024..=1024 * 1024).contains(&self.max_request_bytes)
            && (1024..=16 * 1024 * 1024).contains(&self.max_frame_bytes)
            && (1..=64).contains(&self.max_queued)
            && (1..=64).contains(&self.max_plans)
            && (1..=128).contains(&self.max_tools)
            && (1024..=1024 * 1024 * 1024).contains(&self.max_stderr_bytes)
            && [
                self.initialize_timeout,
                self.request_timeout,
                self.shutdown_timeout,
            ]
            .into_iter()
            .all(|d| (Duration::from_millis(1)..=Duration::from_secs(3600)).contains(&d))
    }
}
#[derive(Clone)]
#[non_exhaustive]
pub struct ProcessConfig {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub workspace: PathBuf,
    pub namespace: String,
    pub environment: BTreeMap<String, String>,
    pub aliases: BTreeMap<String, String>,
    pub capabilities: BTreeMap<String, ToolCapability>,
    pub expected_server: Option<ServerInfo>,
    pub limits: ProcessLimits,
}
impl ProcessConfig {
    pub(crate) fn valid(&self) -> bool {
        self.limits.valid()
            && self.program.is_absolute()
            && self.program.as_os_str().as_encoded_bytes().len() <= 4096
            && !self.program.as_os_str().as_encoded_bytes().contains(&0)
            && self.workspace.as_os_str().as_encoded_bytes().len() <= 4096
            && !self.workspace.as_os_str().as_encoded_bytes().contains(&0)
            && valid_name(&self.namespace)
            && self.namespace.len() <= 24
            && self.arguments.len() <= 128
            && self.arguments.iter().map(String::len).sum::<usize>() <= 64 * 1024
            && !self.arguments.iter().any(|v| v.contains('\0'))
            && self.environment.len() <= 128
            && self
                .environment
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
                <= 64 * 1024
            && self.environment.iter().all(|(key, value)| {
                !key.is_empty()
                    && key.len() <= 256
                    && !value.contains('\0')
                    && key.bytes().enumerate().all(|(i, b)| {
                        b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                    })
            })
            && self.aliases.len() <= self.limits.max_tools
            && self.capabilities.len() <= self.limits.max_tools
            && self
                .aliases
                .iter()
                .all(|(key, value)| valid_name(key) && valid_name(value))
            && self.capabilities.keys().all(|key| valid_name(key))
            && self.expected_server.as_ref().is_none_or(|s| {
                valid_name(&s.id)
                    && !s.version.is_empty()
                    && s.version.len() <= 128
                    && s.version.bytes().all(|b| b.is_ascii_graphic())
            })
    }
    pub fn new(
        program: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
        namespace: impl Into<String>,
    ) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            workspace: workspace.into(),
            namespace: namespace.into(),
            environment: BTreeMap::new(),
            aliases: BTreeMap::new(),
            capabilities: BTreeMap::new(),
            expected_server: None,
            limits: Default::default(),
        }
    }
}
impl fmt::Debug for ProcessConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessConfig")
            .field("program", &self.program)
            .field("workspace", &self.workspace)
            .field("namespace", &self.namespace)
            .field("argument_count", &self.arguments.len())
            .field(
                "environment_names",
                &self.environment.keys().collect::<Vec<_>>(),
            )
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProcessErrorKind {
    InvalidConfiguration,
    StartFailed,
    Closed,
    Busy,
    InputLimit,
    FrameLimit,
    StderrLimit,
    Protocol,
    Exited,
    Io,
    Timeout,
    Cancelled,
    Remote,
    InvalidCatalog,
    InvalidResult,
    Workspace,
    PlanLimit,
    DisposeFailed,
    Shutdown,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProcessDiagnostics {
    pub pid: Option<u32>,
    pub closed: bool,
    pub worker_done: bool,
    pub reaped: bool,
    pub cleanup_pending: bool,
    pub stop: Option<ProcessErrorKind>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub requests_sent: u64,
    pub completed_requests: u64,
}

#[derive(Debug, Clone)]
pub struct ProcessError {
    pub kind: ProcessErrorKind,
    pub may_have_executed: bool,
    pub remote_code: Option<i64>,
    pub diagnostics: ProcessDiagnostics,
}
impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "external tool {:?}; execution may have occurred: {}",
            self.kind, self.may_have_executed
        )
    }
}
impl std::error::Error for ProcessError {}
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
