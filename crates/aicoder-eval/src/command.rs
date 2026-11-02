//! Trusted graders reuse runtime's host-owned native argv, capture and process-group owner.
use aicoder_core::{TurnExecutionContext, tools::ToolContext};
use aicoder_runtime::{
    process::{ProcessCaptureOptions, capture as capture_process},
    tools::ShellOutput,
};
use anyhow::{Result, ensure};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
    process::Command,
    time::Duration,
};
pub(crate) async fn capture(
    program: &OsStr,
    args: &[OsString],
    workspace: &Path,
    duration: Duration,
    output_limit: usize,
    excluded: &[String],
) -> Result<ShellOutput> {
    ensure!(
        !duration.is_zero() && duration <= Duration::from_secs(600),
        "grader deadline must be 1 ns to 600 s"
    );
    ensure!(args.len() <= 256, "grader argument count budget exceeded");
    let mut command = Command::new(program);
    command.args(args);
    let context = ToolContext::new(workspace, duration, output_limit.clamp(1024, 65536))?;
    let mut options = ProcessCaptureOptions::default();
    options.excluded_environment = excluded.to_vec();
    capture_process(command, &context, options, TurnExecutionContext::new()).await
}
