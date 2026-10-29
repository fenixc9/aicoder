mod bash;
mod edit_file;
mod grep;
mod read_file;
mod write_file;

use anyhow::Result;

pub use aicoder_core::tools::{
    AllowAllApproval, ApprovalHandler, DenyAllApproval, DispatcherConfig, ExecutableTool,
    ToolCapability, ToolContext, ToolDispatcher, ToolFailure, ToolInvocation, ToolRegistry,
    ToolSuccess,
};
mod util;
mod workspace;
pub use workspace::WorkspacePaths;

pub use bash::BashTool;
pub use edit_file::EditFileTool;
pub use grep::GrepTool;
pub use read_file::ReadFileTool;
pub use write_file::WriteFileTool;

/// Default coding tools without application settings or resource discovery.
pub fn default_registry() -> Result<ToolRegistry> {
    registry_with_shell(BashTool)
}

pub(crate) fn registry_with_shell(shell: impl ExecutableTool + 'static) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::default();
    registry.register(ReadFileTool)?;
    registry.register(WriteFileTool)?;
    registry.register(EditFileTool)?;
    registry.register(shell)?;
    registry.register(GrepTool)?;
    Ok(registry)
}

#[cfg(test)]
mod tests;
