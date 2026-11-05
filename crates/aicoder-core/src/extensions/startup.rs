//! Once-per-run startup edits, distinct from request-local views and raw-input interception.
use super::*;
use crate::{ExecutionInput, model::*};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub struct BeforeStartInput<'a> {
    pub trigger: Option<&'a crate::RunTrigger>,
    pub workspace: &'a Path,
    pub request: &'a ModelRequest,
    pub history: &'a [Message],
    pub system_prefix_len: usize,
    pub entries: &'a [BoundaryProposal],
    /// The immutable host-admitted root catalog; leaf tools are never exposed implicitly.
    pub available_tools: &'a [ToolDefinition],
}
#[derive(Debug, Default)]
pub struct BeforeStartAction {
    /// Replace only this run's declared runtime prefix, preserving all recorded history.
    pub system_prefix: Option<Vec<Message>>,
    /// Select any unique subset of the immutable host-admitted root catalog, including empty.
    pub tools: Option<Vec<String>>,
    pub append: Vec<BoundaryEntry>,
}
#[async_trait]
pub trait BeforeStartHook: Send + Sync {
    async fn before_start(
        &self,
        input: BeforeStartInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<Option<BeforeStartAction>>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupChange {
    pub extension: String,
    pub hook: String,
    pub prefix_changed: bool,
    pub tools_changed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupMetadata {
    pub system_prefix_messages: usize,
    pub tools: Vec<String>,
    pub changes: Vec<StartupChange>,
}
#[derive(Debug, Clone)]
pub(crate) struct PreparedStartup {
    pub owner: uuid::Uuid,
    pub commit: BoundaryCommit,
    pub encoded: String,
    pub committed: bool,
}
impl ExecutionInput {
    /// Opt into once-per-run startup preparation, rather than a model-selection preview.
    /// Agent turn entry points set this automatically. Prepared values stay executor-bound.
    pub fn for_run(mut self) -> Self {
        self.start_requested = true;
        self
    }
}
