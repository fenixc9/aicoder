//! Tool interfaces, registry, dispatching, approval and host execution context.

mod approval;
mod context;
mod core;
mod dispatcher;
mod registry;
mod util;

pub use approval::{
    AllowAllApproval, ApprovalHandler, DenyAllApproval, MAX_TOOL_PREVIEW_BYTES, ToolInvocation,
    ToolPreview,
};
pub use context::ToolContext;
pub use core::{ExecutableTool, PreparedTool, ToolCapability, ToolFailure, ToolSuccess};
pub use dispatcher::{DispatcherConfig, ToolDispatcher};
pub use registry::ToolRegistry;

#[cfg(test)]
mod tests;

pub(crate) use dispatcher::{DispatchCapture, DispatchJournal};
