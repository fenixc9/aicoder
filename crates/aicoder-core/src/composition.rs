//! Host-owned bounded tool programs. Engines emit intents; core retains dispatch authority.
use crate::{
    model::{Message, ModelToolCall, ToolDefinition},
    tools::{ToolFailure, ToolSuccess},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CompositionLimits {
    pub max_calls: usize,
    pub max_parallel: usize,
    pub max_request_bytes: usize,
    pub max_result_bytes: usize,
    pub max_state_bytes: usize,
    pub duration: Duration,
}
impl Default for CompositionLimits {
    fn default() -> Self {
        Self {
            max_calls: 32,
            max_parallel: 4,
            max_request_bytes: 64 * 1024,
            max_result_bytes: 64 * 1024,
            max_state_bytes: 64 * 1024,
            duration: Duration::from_secs(30),
        }
    }
}
impl CompositionLimits {
    pub(crate) fn validate(self) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (1..=64).contains(&self.max_calls)
                && (1..=8).contains(&self.max_parallel)
                && (512..=64 * 1024).contains(&self.max_request_bytes)
                && (512..=256 * 1024).contains(&self.max_result_bytes)
                && (1024..=256 * 1024).contains(&self.max_state_bytes)
                && (Duration::from_millis(1)..=Duration::from_secs(600)).contains(&self.duration),
            "invalid tool composition limits"
        );
        Ok(self)
    }
}
pub struct CompositionContext {
    pub tools: Vec<ToolDefinition>,
    pub state: Option<Value>,
}
#[derive(Debug, Clone)]
pub struct ProgramCall {
    pub id: u64,
    pub tool: String,
    pub arguments: Value,
}
#[derive(Debug, Clone)]
pub struct ProgramResponse {
    pub id: u64,
    pub content: Value,
    pub is_error: bool,
}
pub struct ProgramOutput {
    pub result: ToolSuccess,
    pub state: Option<Value>,
}
pub enum ProgramEvent {
    Call(ProgramCall),
    Finished(ProgramOutput),
    Failed(ToolFailure),
}
/// Trusted native registration, never model-supplied authority. No direct tool IO is provided.
pub trait ToolCompositionFactory: Send + Sync {
    /// None offers every non-composite tool in the frozen host registry; Some is an exact subset.
    fn allowed_tools(&self) -> Option<&[String]>;
    fn limits(&self) -> CompositionLimits {
        CompositionLimits::default()
    }
    fn state_namespace(&self) -> Option<&str> {
        None
    }
    fn start(
        &self,
        context: CompositionContext,
        arguments: Value,
    ) -> Result<Box<dyn ToolProgram>, ToolFailure>;
}
#[async_trait]
pub trait ToolProgram: Send {
    /// Cancellation-safe; dropped waits must leave the next queued event available.
    async fn next(&mut self) -> Result<ProgramEvent, ToolFailure>;
    fn respond(&mut self, response: ProgramResponse) -> Result<(), ToolFailure>;
    fn cancel(&mut self);
    /// Stop promptly; core discards further intents after cancellation and bounds cleanup independently.
    async fn shutdown(&mut self) -> Result<(), ToolFailure>;
}
pub const COMPOSITION_NAMESPACE: &str = "aicoder.composition.v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompositionRecord {
    Started {
        scope: String,
        parent_call_id: String,
    },
    Call {
        scope: String,
        sequence: u64,
        call: ModelToolCall,
    },
    Result {
        scope: String,
        sequence: u64,
        message: Message,
    },
    Finished {
        scope: String,
        status: String,
        abandoned: Vec<u64>,
    },
}
