//! Explicit host-owned process capture. This API installs no model tool or authorization rule.
use crate::tools::{ShellLimits, ShellOutput};
use aicoder_core::{TurnExecutionContext, tools::ToolContext};
use std::{process::Command, time::Duration};
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProcessCaptureOptions {
    pub limits: ShellLimits,
    pub archive_timeout: Duration,
    pub excluded_environment: Vec<String>,
}
impl Default for ProcessCaptureOptions {
    fn default() -> Self {
        Self {
            limits: ShellLimits::default(),
            archive_timeout: Duration::from_millis(250),
            excluded_environment: Vec::new(),
        }
    }
}
/// Override cwd/stdio/group from the owned context, scrub credential references, drain bounded
/// output and await bounded group signalling/reaping on exit, deadline or cancellation. Raw
/// archives are finalized when a store is offered, independently of UTF-8 preview truncation.
/// Dropping this future still signals cleanup independently of the async runtime.
pub async fn capture(
    command: Command,
    context: &ToolContext,
    options: ProcessCaptureOptions,
    cancellation: TurnExecutionContext,
) -> anyhow::Result<ShellOutput> {
    crate::tools::capture_host(command, context, options, cancellation).await
}
