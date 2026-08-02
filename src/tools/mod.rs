//! Tool interfaces, registry, dispatching, approval, and built-in coding tools.

mod approval;
mod builtins;
mod context;
mod core;
mod dispatcher;
mod registry;
mod util;

pub use approval::{AllowAllApproval, ApprovalHandler, ConsoleApproval, ToolInvocation};
// Keep the original `tools::XxxTool` facade stable even when the binary only uses the registry.
#[allow(unused_imports)]
pub use builtins::{
    BashTool, EditFileTool, GrepTool, ReadFileTool, WriteFileTool, default_registry,
};
pub use context::ToolContext;
pub use core::{ExecutableTool, ToolCapability, ToolFailure, ToolSuccess};
pub use dispatcher::{DispatcherConfig, ToolDispatcher};
pub use registry::ToolRegistry;

#[cfg(test)]
mod tests;
