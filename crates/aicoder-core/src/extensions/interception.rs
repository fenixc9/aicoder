//! Owned tool interception. Recorded proposals never restore execution or storage authority.
use super::{hooks::Registered, *};
use crate::{artifacts::ArtifactRef, model::*, tools::*};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

pub const INTERCEPTION_NAMESPACE: &str = "aicoder.interception.v1";
pub const MAX_INTERCEPTION_BYTES: usize = 64 * 1024;
pub const MAX_INTERCEPTION_CHAIN_BYTES: usize = 256 * 1024;
pub struct ToolCallHookInput<'a> {
    pub workspace: &'a Path,
    pub original: &'a ModelToolCall,
    pub call: &'a ModelToolCall,
    pub capability: ToolCapability,
}
#[derive(Debug)]
pub enum ToolCallEdit {
    Continue,
    Arguments(Value),
    Deny { reason: String },
}
#[async_trait]
pub trait ToolCallHook: Send + Sync {
    /// Native host transformation before guards, preparation and approval; identity is immutable.
    async fn before_call(
        &self,
        input: ToolCallHookInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<ToolCallEdit>;
}
pub struct ToolResultHookInput<'a> {
    pub workspace: &'a Path,
    pub call: &'a ModelToolCall,
    /// Bounded delivered envelope; later handlers see prior replacements.
    pub envelope: &'a Value,
    /// Actual execution status remains immutable even if the delivered error status changes.
    pub execution: ToolExecutionStatus,
    pub operation_completed: Option<bool>,
}
#[derive(Debug)]
pub struct ToolResultEdit {
    pub is_error: bool,
    /// Success output, or an error object with code/message and optional details.
    pub payload: Value,
    /// Only exact references already exposed by the preceding envelope may be retained.
    pub artifacts: Vec<ArtifactRef>,
}
#[async_trait]
pub trait ToolResultHook: Send + Sync {
    async fn after_tool(
        &self,
        input: ToolResultHookInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<Option<ToolResultEdit>>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Succeeded,
    Failed,
    TimedOut,
    ApprovalDenied,
    Cancelled,
}
impl ToolExecutionStatus {
    pub(crate) fn of(outcome: &crate::events::ToolExecutionOutcome) -> Self {
        use crate::events::ToolExecutionOutcome as O;
        match outcome {
            O::Succeeded { .. } => Self::Succeeded,
            O::Failed { .. } => Self::Failed,
            O::TimedOut => Self::TimedOut,
            O::ApprovalDenied => Self::ApprovalDenied,
            O::Cancelled => Self::Cancelled,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InterceptionRecord {
    /// A proposed argument change, not evidence of preparation, approval or execution.
    Arguments {
        call_id: String,
        tool: String,
        extension: String,
        hook: String,
        arguments: Value,
    },
    /// Durable before tool execution; an absent Finished retains delivery uncertainty.
    ResultPending { call_id: String, tool: String },
    ResultFinished {
        call_id: String,
        tool: String,
        execution: ToolExecutionStatus,
        operation_completed: Option<bool>,
        original_truncated: bool,
        original_capture_failed: bool,
        original_artifacts: Vec<ArtifactRef>,
        delivered_is_error: bool,
        delivery_complete: bool,
        changes: Vec<InterceptionChange>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterceptionChange {
    pub extension: String,
    pub hook: String,
    pub is_error: bool,
    pub artifacts: Vec<ArtifactRef>,
}

pub(crate) fn replace_result(
    envelope: &mut Value,
    edit: ToolResultEdit,
    limit: usize,
) -> anyhow::Result<()> {
    size(&edit.payload, limit.min(MAX_INTERCEPTION_BYTES))?;
    anyhow::ensure!(edit.artifacts.len() <= 8, "result artifact limit exceeded");
    let old: Vec<ArtifactRef> = envelope
        .get("artifacts")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()?
        .unwrap_or_default();
    let mut ids = std::collections::HashSet::new();
    anyhow::ensure!(
        edit.artifacts
            .iter()
            .all(|r| r.validate().is_ok() && old.contains(r) && ids.insert(&r.id)),
        "result hooks cannot add or alter artifact authority"
    );
    if edit.is_error {
        let object = edit
            .payload
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("invalid result error"))?;
        anyhow::ensure!(
            object
                .keys()
                .all(|k| matches!(k.as_str(), "code" | "message" | "details"))
                && object
                    .get("code")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty()
                        && s.len() <= 128
                        && !s.chars().any(char::is_control))
                && object
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.len() <= 4096),
            "invalid result error"
        );
        envelope
            .as_object_mut()
            .expect("host envelope")
            .remove("output");
        envelope["error"] = edit.payload;
    } else {
        envelope
            .as_object_mut()
            .expect("host envelope")
            .remove("error");
        envelope["output"] = edit.payload;
    }
    envelope["ok"] = Value::Bool(!edit.is_error);
    envelope["artifacts"] = serde_json::to_value(edit.artifacts)?;
    // A replacement owns its complete delivered payload; original execution/capture flags
    // survive in the immutable interception receipt, not stale JSON-prefix presentation hints.
    for field in ["preview_format", "truncated"] {
        envelope
            .as_object_mut()
            .expect("host envelope")
            .remove(field);
    }
    Ok(())
}
pub(crate) fn ordered<T: ?Sized>(sets: impl Iterator<Item = Registered<T>>) -> Vec<Registered<T>> {
    let mut hooks: Vec<_> = sets.collect();
    hooks.sort_by_key(|h| h.spec.priority);
    hooks
}
