//! Bounded shell capture and process-group ownership through exit, timeout and drop.
mod capture;
mod process;
use super::{
    ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess, util::parse_arguments,
};
use aicoder_core::model::ToolDefinition;
use async_trait::async_trait;
use capture::Captures;
use process::{ManagedChild, Stop};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::Duration,
};

pub use process::Stop as ShellStopReason;

/// Structured text capture returned as tool output, or as failure details.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ShellOutput {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_omitted_bytes: u64,
    pub stderr_omitted_bytes: u64,
    pub truncated: bool,
    pub output_complete: bool,
    pub stop_reason: ShellStopReason,
    pub process_group_signal_requested: bool,
    pub reaped: bool,
    pub cleanup_ok: bool,
    pub encoding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_artifact: Option<aicoder_core::artifacts::ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_artifact: Option<aicoder_core::artifacts::ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_artifact_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_artifact_error: Option<String>,
}

pub struct BashTool;
/// Host-owned limits, independent of the serialized tool-result cap.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShellLimits {
    pub max_command_bytes: usize,
    pub max_output_bytes: u64,
}
impl Default for ShellLimits {
    fn default() -> Self {
        Self {
            max_command_bytes: 64 * 1024,
            max_output_bytes: 16 * 1024 * 1024,
        }
    }
}
impl ShellLimits {
    fn validate(self) -> Result<(), ToolFailure> {
        if !(1..=1024 * 1024).contains(&self.max_command_bytes)
            || !(1..=1024 * 1024 * 1024).contains(&self.max_output_bytes)
        {
            return Err(ToolFailure::new(
                "invalid_shell_configuration",
                "Shell command limit must be 1..1 MiB and output scan limit 1..1 GiB",
            ));
        }
        Ok(())
    }
}
pub struct ConfiguredBash {
    archive_timeout: Duration,
    limits: ShellLimits,
    variables: Vec<String>,
}
impl ConfiguredBash {
    /// Trusted host budget per archive IO operation; independent of model shell arguments.
    pub fn with_archive_timeout(mut self, timeout: Duration) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (Duration::from_millis(1)..=Duration::from_secs(5)).contains(&timeout),
            "Archive timeout must be 1 ms to 5 s"
        );
        self.archive_timeout = timeout;
        Ok(self)
    }

    pub fn excluding_environment(mut self, variables: Vec<String>) -> Self {
        self.variables = variables;
        self
    }
}
impl BashTool {
    pub fn excluding_environment(variables: Vec<String>) -> ConfiguredBash {
        ConfiguredBash {
            archive_timeout: Duration::from_millis(250),
            limits: ShellLimits::default(),
            variables,
        }
    }
    pub fn with_limits(limits: ShellLimits) -> anyhow::Result<ConfiguredBash> {
        limits
            .validate()
            .map_err(|error| anyhow::anyhow!(error.message))?;
        Ok(ConfiguredBash {
            archive_timeout: Duration::from_millis(250),
            limits,
            variables: Vec::new(),
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    command: String,
    timeout_seconds: Option<u64>,
}
fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "bash".into(),
        description: Some("Run non-interactive Bash from the workspace with bounded prefix/tail output. Commands run with user permissions, not in a sandbox. Same-group background jobs are terminated when the command ends. timeout_seconds is capped by the host tool deadline.".into()),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout_seconds": {
                    "type": "integer", "minimum": 1, "maximum": 86400,
                    "description": "Optional active deadline in seconds; defaults to the host tool deadline"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
    }
}

#[async_trait]
impl ExecutableTool for BashTool {
    fn definition(&self) -> ToolDefinition {
        definition()
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Command
    }
    async fn execute(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        execute(
            context,
            args,
            ShellLimits::default(),
            &[],
            Duration::from_millis(250),
        )
        .await
    }
}
#[async_trait]
impl ExecutableTool for ConfiguredBash {
    fn definition(&self) -> ToolDefinition {
        definition()
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Command
    }
    async fn execute(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        execute(
            context,
            args,
            self.limits,
            &self.variables,
            self.archive_timeout,
        )
        .await
    }
}
async fn execute(
    context: &ToolContext,
    args: Value,
    limits: ShellLimits,
    excluded: &[String],
    archive_timeout: Duration,
) -> Result<ToolSuccess, ToolFailure> {
    limits.validate()?;
    let args: Arguments = parse_arguments(args)?;
    if args.command.is_empty()
        || args.command.len() > limits.max_command_bytes
        || args.command.contains('\0')
        || args
            .timeout_seconds
            .is_some_and(|seconds| !(1..=86_400).contains(&seconds))
    {
        return Err(ToolFailure::new(
            "invalid_arguments",
            "Nonempty command must fit the host command limit, contain no NUL, and timeout_seconds must be 1..86400",
        ));
    }
    if excluded.iter().any(|name| !valid_variable(name)) {
        return Err(ToolFailure::new(
            "invalid_shell_configuration",
            "Credential exclusions must be environment variable names",
        ));
    }
    let output_limit = context.max_output_bytes().min(64 * 1024);
    if output_limit < 1024 {
        return Err(ToolFailure::new(
            "output_limit_too_small",
            "Shell output budget must be at least 1024 bytes for a complete result",
        ));
    }
    let mut captures = Captures::new(output_limit / 2)
        .with_store(context.artifact_store().cloned())
        .with_archive_timeout(archive_timeout);
    if context.tool_timeout().is_zero() {
        return finish(
            captures,
            process::Completion::tool_timeout(false),
            output_limit,
            false,
        )
        .await;
    }
    let work = run(context, &args, limits, excluded, &mut captures);
    let completion = match tokio::time::timeout(context.tool_timeout(), work).await {
        Ok(Ok(completion)) => completion,
        Ok(Err(error)) => return Err(error),
        Err(_) => process::Completion::tool_timeout(captures.spawned),
    };
    finish(captures, completion, output_limit, false).await
}
async fn run(
    context: &ToolContext,
    args: &Arguments,
    limits: ShellLimits,
    excluded: &[String],
    captures: &mut Captures,
) -> Result<process::Completion, ToolFailure> {
    let requested = args
        .timeout_seconds
        .map(Duration::from_secs)
        .unwrap_or(context.tool_timeout());
    let mut command = Command::new("/bin/bash");
    command
        .args(["--noprofile", "--norc", "-c"])
        .arg(&args.command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(context.workspace_root())
        .process_group(0);
    run_command(
        context, command, limits, excluded, captures, requested, None,
    )
    .await
}
async fn run_command(
    context: &ToolContext,
    mut command: Command,
    limits: ShellLimits,
    excluded: &[String],
    captures: &mut Captures,
    requested: Duration,
    cancellation: Option<&aicoder_core::TurnExecutionContext>,
) -> Result<process::Completion, ToolFailure> {
    let mut exits = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())
        .map_err(start_error)?;
    let deadline = tokio::time::Instant::now()
        .checked_add(requested)
        .ok_or_else(|| ToolFailure::new("invalid_arguments", "Process deadline is too large"))?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(context.workspace_root())
        .process_group(0);
    scrub_credentials(
        &mut command,
        std::env::var("AICODER_API_KEY_ENV").ok().as_deref(),
    );
    for variable in excluded {
        command.env_remove(variable);
    }
    let mut managed = ManagedChild::new(command.spawn().map_err(start_error)?);
    captures.spawned = true;
    let mut stdout = tokio::process::ChildStdout::from_std(
        managed.child_mut().stdout.take().expect("piped stdout"),
    )
    .map_err(start_error)?;
    let mut stderr = tokio::process::ChildStderr::from_std(
        managed.child_mut().stderr.take().expect("piped stderr"),
    )
    .map_err(start_error)?;
    let stop = capture::drive(
        &mut managed,
        &mut exits,
        &mut stdout,
        &mut stderr,
        captures,
        capture::DriveOptions {
            limit: limits.max_output_bytes,
            deadline,
            cancellation,
        },
    )
    .await;
    let signalled = managed.signal_group();
    let completion = managed.reap(&mut exits, stop, signalled).await;
    // Reaping the group leader is independent of inherited pipes. Drain only within
    // the remaining scan budget and a bounded cleanup interval after group termination.
    if completion.reaped
        && !matches!(
            completion.stop,
            Stop::OutputLimit | Stop::CaptureError | Stop::WaitError
        )
    {
        capture::drain(&mut stdout, &mut stderr, captures, limits.max_output_bytes).await;
    }
    Ok(completion)
}
pub(crate) async fn capture_host(
    command: Command,
    context: &ToolContext,
    options: crate::process::ProcessCaptureOptions,
    cancellation: aicoder_core::TurnExecutionContext,
) -> anyhow::Result<ShellOutput> {
    options
        .limits
        .validate()
        .map_err(|e| anyhow::anyhow!(e.message))?;
    anyhow::ensure!(
        (Duration::from_millis(1)..=Duration::from_secs(86400)).contains(&context.tool_timeout())
            && (1024..=65536).contains(&context.max_output_bytes()),
        "invalid host process duration/preview limits"
    );
    anyhow::ensure!(
        (Duration::from_millis(1)..=Duration::from_secs(5)).contains(&options.archive_timeout),
        "invalid host archive timeout"
    );
    anyhow::ensure!(
        options.excluded_environment.len() <= 256
            && options
                .excluded_environment
                .iter()
                .all(|name| valid_variable(name)),
        "invalid host credential references"
    );
    let count = std::iter::once(command.get_program())
        .chain(command.get_args())
        .try_fold(0usize, |used, arg| {
            used.checked_add(arg.as_encoded_bytes().len() + 1)
        })
        .ok_or_else(|| anyhow::anyhow!("process argument size overflow"))?;
    anyhow::ensure!(
        count <= options.limits.max_command_bytes,
        "host process argument byte budget exceeded"
    );
    let mut captures = Captures::new(context.max_output_bytes() / 2)
        .with_store(context.artifact_store().cloned())
        .with_archive_timeout(options.archive_timeout);
    let completion = if cancellation.is_cancelled() {
        process::Completion {
            stop: Stop::Cancelled,
            exit_code: None,
            signal: None,
            signal_requested: false,
            reaped: false,
            cleanup_ok: true,
        }
    } else {
        run_command(
            context,
            command,
            options.limits,
            &options.excluded_environment,
            &mut captures,
            context.tool_timeout(),
            Some(&cancellation),
        )
        .await
        .map_err(|failure| anyhow::anyhow!(failure.code))?
    };
    let value = match finish(
        captures,
        completion,
        context.max_output_bytes(),
        context.artifact_store().is_some(),
    )
    .await
    {
        Ok(result) => result.output,
        Err(failure) => failure
            .details
            .ok_or_else(|| anyhow::anyhow!("host capture failed: {}", failure.code))?,
    };
    Ok(serde_json::from_value(value)?)
}
fn start_error(error: std::io::Error) -> ToolFailure {
    ToolFailure::new(
        "command_failed",
        format!("Cannot start shell capture: {error}"),
    )
}
async fn finish(
    mut captures: Captures,
    mut completion: process::Completion,
    limit: usize,
    archive_all: bool,
) -> Result<ToolSuccess, ToolFailure> {
    if captures.limit_hit {
        completion.stop = Stop::OutputLimit;
    } else if matches!(completion.stop, Stop::Exit) && completion.cleanup_ok && !captures.complete()
    {
        completion.stop = Stop::CaptureError;
    }
    let initial = output_for(&captures, &completion, limit);
    if archive_all || initial["truncated"] == true {
        captures.stdout.finalize_archive().await;
        captures.stderr.finalize_archive().await;
    }
    let artifacts = captures
        .stdout
        .artifact
        .iter()
        .chain(captures.stderr.artifact.iter())
        .cloned()
        .collect::<Vec<_>>();
    let details = output_for(&captures, &completion, limit);
    if details.to_string().len() > limit {
        return Err(ToolFailure::new(
            "output_limit_too_small",
            "Output budget cannot hold the shell result",
        ));
    }
    if !completion.cleanup_ok && matches!(completion.stop, Stop::Exit) {
        let mut failure = ToolFailure::new(
            "cleanup_failed",
            "Shell process cleanup could not be confirmed; inspect the workspace before retrying",
        )
        .with_details(details);
        failure.artifacts = artifacts;
        return Err(failure);
    }
    let failure = match completion.stop {
        Stop::Timeout => Some((
            "timeout",
            "Command exceeded its active time limit; partial output retained",
        )),
        Stop::Cancelled => Some((
            "cancelled",
            "Host process was cancelled; completed operations were not repeated",
        )),
        Stop::ToolTimeout => Some((
            "timeout",
            "Shell tool exceeded the host deadline; cleanup requested and partial output retained",
        )),
        Stop::OutputLimit => Some((
            "output_limit",
            "Command exceeded the combined stdout/stderr scan limit",
        )),
        Stop::CaptureError => Some((
            "capture_failed",
            "Shell output could not be completely captured; inspect before retrying",
        )),
        Stop::WaitError => Some((
            "process_wait_failed",
            "Shell child exit could not be observed",
        )),
        Stop::Exit if completion.exit_code != Some(0) => {
            Some(("command_failed", "Command exited unsuccessfully"))
        }
        Stop::Exit => None,
    };
    if let Some((code, message)) = failure {
        let mut failure = ToolFailure::new(code, message).with_details(details);
        failure.artifacts = artifacts;
        return Err(failure);
    }
    Ok(ToolSuccess {
        artifacts,
        truncated: details["truncated"] == true,
        output: details,
    })
}
fn output_for(captures: &Captures, completion: &process::Completion, limit: usize) -> Value {
    let make = |bytes: usize| {
        let mut out_quota = bytes / 2;
        let mut err_quota = bytes - out_quota;
        if captures.stderr.seen < err_quota as u64 {
            out_quota += err_quota - captures.stderr.seen as usize;
            err_quota = captures.stderr.seen as usize;
        } else if captures.stdout.seen < out_quota as u64 {
            err_quota += out_quota - captures.stdout.seen as usize;
            out_quota = captures.stdout.seen as usize;
        }
        let stdout = captures.stdout.view(out_quota);
        let stderr = captures.stderr.view(err_quota);
        serde_json::to_value(ShellOutput {
            exit_code: completion.exit_code,
            signal: completion.signal,
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_bytes: captures.stdout.seen,
            stderr_bytes: captures.stderr.seen,
            stdout_omitted_bytes: stdout.omitted,
            stderr_omitted_bytes: stderr.omitted,
            truncated: stdout.omitted > 0 || stderr.omitted > 0 || !captures.complete(),
            output_complete: captures.complete(),
            stop_reason: completion.stop,
            process_group_signal_requested: completion.signal_requested,
            reaped: completion.reaped,
            cleanup_ok: completion.cleanup_ok,
            encoding: "utf8_lossy".into(),
            stdout_artifact: captures.stdout.artifact.clone(),
            stderr_artifact: captures.stderr.artifact.clone(),
            stdout_artifact_error: captures.stdout.artifact_error.clone(),
            stderr_artifact_error: captures.stderr.artifact_error.clone(),
        })
        .expect("shell result is serializable")
    };
    let (mut low, mut high) = (0, limit);
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if make(mid).to_string().len() <= limit {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    make(low)
}
fn valid_variable(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn scrub_credentials(command: &mut Command, referenced: Option<&str>) {
    for name in [
        "OPENAI_API_KEY",
        "DEEPSEEK_API_KEY",
        "AICODER_API_KEY",
        "AICODER_API_KEY_ENV",
        "AICODER_PROXY",
        "ANTHROPIC_API_KEY",
        "DASHSCOPE_API_KEY",
        "ZHIPU_API_KEY",
        "BASH_ENV",
        "ENV",
        "SHELLOPTS",
        "BASHOPTS",
        "CDPATH",
        "GLOBIGNORE",
        "BASH_XTRACEFD",
        "BASH_COMPAT",
        "POSIXLY_CORRECT",
    ] {
        command.env_remove(name);
    }
    if let Some(name) = referenced.filter(|name| valid_variable(name)) {
        command.env_remove(name);
    }
    // Only names are inspected; values are never copied to diagnostics or results.
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"BASH_FUNC_") {
            command.env_remove(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn command_output_is_captured_and_stdin_is_closed() {
        let workspace = tempfile::tempdir().unwrap();
        let context = ToolContext::new(workspace.path(), Duration::from_secs(2), 4096).unwrap();
        let result=BashTool.execute(&context,json!({"command":"printf 'captured stdout'; printf 'captured stderr' >&2; if read -r line; then exit 9; fi"})).await.unwrap();
        assert_eq!(result.output["stdout"], "captured stdout");
        assert_eq!(result.output["stderr"], "captured stderr");
        assert_eq!(result.output["exit_code"], 0);
    }
    #[test]
    fn managed_and_referenced_credentials_are_removed_from_the_child_environment() {
        let mut command = Command::new("/bin/bash");
        command
            .env("AICODER_API_KEY", "fake-secret")
            .env("SERVICE_TOKEN", "fake-secret");
        scrub_credentials(&mut command, Some("SERVICE_TOKEN"));
        for name in [
            "OPENAI_API_KEY",
            "DEEPSEEK_API_KEY",
            "AICODER_API_KEY",
            "SERVICE_TOKEN",
            "BASH_ENV",
        ] {
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == name && value.is_none())
            );
        }
    }
}
