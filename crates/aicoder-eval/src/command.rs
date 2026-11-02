//! Reuse runtime's bounded capture and owned process-group cleanup for trusted graders.
use aicoder_runtime::tools::{BashTool, ExecutableTool, ShellLimits, ShellOutput, ToolContext};
use anyhow::{Result, ensure};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
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
    let mut command = String::from("exec");
    for value in std::iter::once(program).chain(args.iter().map(OsString::as_os_str)) {
        let value = value
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("grader arguments must be UTF-8"))?;
        ensure!(value.len() <= 65536, "grader argument byte budget exceeded");
        let quoted =
            shlex::try_quote(value).map_err(|_| anyhow::anyhow!("grader argument contains NUL"))?;
        ensure!(
            command.len() + quoted.len() < 65536,
            "grader command byte budget exceeded"
        );
        command.push(' ');
        command.push_str(&quoted);
    }
    let tool =
        BashTool::with_limits(ShellLimits::default())?.excluding_environment(excluded.to_vec());
    // Let the runtime's own deadline perform and await process-group cleanup.
    let context = ToolContext::new(workspace, duration, output_limit.clamp(1024, 65536))?;
    let output = match tool
        .execute(&context, serde_json::json!({"command":command}))
        .await
    {
        Ok(result) => result.output,
        Err(failure) => failure.details.ok_or_else(|| {
            anyhow::anyhow!(
                "grader process could not produce a capture: {}",
                failure.code
            )
        })?,
    };
    Ok(serde_json::from_value(output)?)
}
