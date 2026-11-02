//! Bounded Git IO for isolated benchmark workspaces; no shell command interpolation.
use aicoder_core::{TurnExecutionContext, artifacts::*, tools::ToolContext};
use aicoder_runtime::{
    process::{ProcessCaptureOptions, capture},
    tools::{ShellLimits, ShellStopReason},
};
use anyhow::{Result, ensure};
use std::{ffi::OsString, path::Path, process::Command, sync::Arc, time::Duration};
pub(crate) async fn run(
    workspace: &Path,
    args: Vec<OsString>,
    index: Option<&Path>,
    context: TurnExecutionContext,
    excluded: &[String],
) -> Result<Vec<u8>> {
    let store = Arc::new(MemoryArtifactStore::new(ArtifactLimits {
        max_artifact_bytes: 8 * 1024 * 1024,
        max_total_bytes: 16 * 1024 * 1024,
        max_artifacts: 2,
    })?);
    let tool = ToolContext::new(workspace, Duration::from_secs(120), 65536)?
        .with_artifact_store(store.clone());
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(args);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let mut options = ProcessCaptureOptions::default();
    options.limits = ShellLimits {
        max_command_bytes: 1024 * 1024,
        max_output_bytes: 8 * 1024 * 1024,
    };
    options.excluded_environment = excluded.into();
    options.archive_timeout = Duration::from_secs(2);
    let output = capture(command, &tool, options, context).await?;
    ensure!(
        output.exit_code == Some(0)
            && output.reaped
            && output.cleanup_ok
            && output.output_complete
            && matches!(output.stop_reason, ShellStopReason::Exit),
        "Git operation failed or was incomplete: {}",
        aicoder_core::redaction::sanitize_text(&output.stderr, 4096)
    );
    if output.stdout_bytes == 0 {
        return Ok(Vec::new());
    }
    let reference = output.stdout_artifact.ok_or_else(|| {
        anyhow::anyhow!("Git raw capture unavailable; operation was not repeated")
    })?;
    ensure!(
        reference.complete
            && reference.bytes == output.stdout_bytes
            && reference.bytes <= 8 * 1024 * 1024,
        "Git raw capture incomplete"
    );
    let mut bytes = Vec::with_capacity(reference.bytes as usize);
    let mut offset = 0;
    loop {
        let page = store.read(&reference.id, offset, 65536).await?;
        ensure!(
            page.offset == offset && page.artifact == reference,
            "Git artifact changed"
        );
        bytes.extend(&page.bytes);
        match page.next_offset {
            Some(next) => {
                ensure!(next > offset, "Git artifact cursor stalled");
                offset = next
            }
            None => break,
        }
    }
    ensure!(
        bytes.len() as u64 == reference.bytes,
        "Git artifact length mismatch"
    );
    Ok(bytes)
}
