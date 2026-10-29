mod atomic_file;
mod bash;
mod edit_file;
mod grep;
mod io;
mod list;
mod read_file;
mod tree;
mod write_file;

use anyhow::Result;

pub use aicoder_core::tools::{
    AllowAllApproval, ApprovalHandler, DenyAllApproval, DispatcherConfig, ExecutableTool,
    PreparedTool, ToolCapability, ToolContext, ToolDispatcher, ToolFailure, ToolInvocation,
    ToolPreview, ToolRegistry, ToolSuccess,
};
mod util;
mod workspace;
pub use workspace::WorkspacePaths;

pub use bash::{BashTool, ConfiguredBash, ShellLimits, ShellOutput, ShellStopReason};
pub use edit_file::EditFileTool;
pub use grep::GrepTool;
pub use list::{GlobTool, ListDirectoryTool};
pub use read_file::ReadFileTool;
pub use write_file::WriteFileTool;

/// Default coding tools without application settings or resource discovery.
pub fn default_registry() -> Result<ToolRegistry> {
    registry_with_shell(BashTool)
}

/// Assemble coding tools with a host-supplied shell implementation.
pub fn registry_with_shell(shell: impl ExecutableTool + 'static) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::default();
    registry.register(ReadFileTool)?;
    registry.register(WriteFileTool)?;
    registry.register(EditFileTool)?;
    registry.register(shell)?;
    registry.register(GrepTool)?;
    registry.register(ListDirectoryTool)?;
    registry.register(GlobTool)?;
    Ok(registry)
}

#[cfg(test)]
mod tests;

/// Plain terminal display, with control and direction-override characters escaped.
pub fn display_preview(preview: &ToolPreview) -> String {
    let raw = format!(
        "{}\n{}{}",
        preview.summary,
        preview.body,
        if preview.truncated {
            "\n[Preview truncated; the operation includes the complete prepared change]"
        } else {
            ""
        }
    );
    raw.chars()
        .flat_map(|ch| {
            if ch != '\n'
                && ch != '\t'
                && (ch.is_control() || matches!(ch as u32, 0x202a..=0x202e | 0x2066..=0x2069))
            {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}
