mod bash;
mod edit_file;
mod grep;
mod read_file;
mod write_file;

use anyhow::Result;

use super::ToolRegistry;

pub use bash::BashTool;
pub use edit_file::EditFileTool;
pub use grep::GrepTool;
pub use read_file::ReadFileTool;
pub use write_file::WriteFileTool;

pub fn default_registry() -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::default();
    registry.register(ReadFileTool)?;
    registry.register(WriteFileTool)?;
    registry.register(EditFileTool)?;
    registry.register(BashTool)?;
    registry.register(GrepTool)?;
    Ok(registry)
}
