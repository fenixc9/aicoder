//! Explicit native extension registration. Native code is trusted host code, not a sandbox.
mod input;
pub use input::{
    INPUT_HOOK_NAMESPACE, InputAction, InputChange, InputDelivery, InputExpander, InputHandled,
    InputHook, InputHookInput, InputHookRecord, InputPreparation, InputReceipt,
    MAX_INPUT_HOOK_BYTES,
};
pub(crate) use input::{InputPipeline, PreparedUserInput};
mod startup;
pub(crate) use startup::PreparedStartup;
pub use startup::{
    BeforeStartAction, BeforeStartHook, BeforeStartInput, StartupChange, StartupMetadata,
};
mod boundary;
mod commands;
pub(crate) use boundary::explicit_context_message;
pub use boundary::{
    BoundaryAction, BoundaryChange, BoundaryCommit, BoundaryEntry, BoundaryHook, BoundaryInput,
    BoundaryKind, BoundaryProposal, BoundaryStatistics, LIFECYCLE_NAMESPACE, LifecycleRecord,
    MAX_BOUNDARY_BYTES,
};
mod hooks;
mod interception;
mod request;
pub use interception::{
    INTERCEPTION_NAMESPACE, InterceptionChange, InterceptionRecord, MAX_INTERCEPTION_BYTES,
    MAX_INTERCEPTION_CHAIN_BYTES, ToolCallEdit, ToolCallHook, ToolCallHookInput,
    ToolExecutionStatus, ToolResultEdit, ToolResultHook, ToolResultHookInput,
};
pub(crate) use interception::{ordered, replace_result};
pub use request::{
    MAX_REQUEST_VIEW_BYTES, REQUEST_VIEW_NAMESPACE, RequestChange, RequestEdit, RequestHook,
    RequestHookInput, RequestHookPass, RequestHookScope, RequestMessages, RequestOptionSummary,
    RequestViewRecord, RequestViewStage,
};
pub(crate) use request::{PreparedRequestView, RequestRegistration, apply_edit};
pub mod process_protocol;
mod registry;
pub use commands::{CommandContext, CommandOutput, CommandRequest, CommandSpec, ExtensionCommand};
pub use hooks::{
    ContextHook, ContextHookInput, ExtensionCancellation, HookSpec, ToolHook, ToolHookDecision,
    ToolHookInput,
};
pub use registry::{
    Extension, ExtensionCatalog, ExtensionManifest, ExtensionRegistry, ExtensionSet,
    RegistrationInfo,
};

use anyhow::{Result, ensure};
use serde::Serialize;
use std::io::Write;
pub const EXTENSION_API_VERSION: u32 = 1;
pub const MAX_EXTENSIONS: usize = 32;
pub const MAX_CONTRIBUTIONS: usize = 256;
pub const MAX_METADATA_BYTES: usize = 1024 * 1024;
pub(crate) fn name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid extension registration name"
    );
    Ok(())
}
pub(crate) fn size(value: &impl Serialize, limit: usize) -> Result<usize> {
    struct Count {
        bytes: usize,
        limit: usize,
    }
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("extension payload budget exceeded"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count { bytes: 0, limit };
    serde_json::to_writer(&mut count, value)?;
    Ok(count.bytes)
}

pub(crate) use hooks::{Registered, invoke, tool_hooks};
