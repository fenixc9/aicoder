use std::{fmt, sync::Arc, time::Duration};

use serde_json::Value;
use uuid::Uuid;

use crate::{
    model::{ModelToolCall, ModelUsage, StopReason},
    state::AgentStateTransition,
    tools::ToolCapability,
};

#[derive(Debug, Clone)]
pub struct AgentRawEventEnvelope {
    pub run_id: RunId,
    pub sequence: u64,
    pub round: Option<usize>,
    pub event: AgentRawEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunId(Uuid);

impl RunId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStage {
    Turn,
    Model,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEnd {
    Completed,
    Aborted { reason: Arc<str> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoundOutcome {
    FinalAnswer,
    CompletionRejected,
    ToolCalls { count: usize },
    Failed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionVerificationOutcome {
    Accepted,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ToolCallKey {
    pub response_index: i32,
    pub tool_index: i32,
}

#[derive(Debug, Clone)]
pub enum ToolExecutionOutcome {
    Succeeded { output: Value, truncated: bool },
    Failed { code: String, message: String },
    TimedOut,
    ApprovalDenied,
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum AgentRawEvent {
    AgentStarted {
        model: Arc<str>,
    },
    AgentCompleted {
        rounds: usize,
        usage: Arc<ModelUsage>,
    },
    AgentFailed {
        stage: AgentStage,
        message: Arc<str>,
    },
    AgentAborted {
        reason: Arc<str>,
    },
    AgentStopped {
        stop: Arc<crate::run::RunStop>,
    },
    StateChanged {
        transition: AgentStateTransition,
    },
    RoundStarted,
    RoundCompleted {
        outcome: RoundOutcome,
    },
    ModelRequestStarted,
    ModelResponseStarted,
    ModelRetryScheduled {
        attempt: u32,
        delay: Duration,
        reason: Arc<str>,
    },
    ModelResponseCompleted {
        finish_reason: Option<StopReason>,
    },
    ModelResponseFailed {
        message: Arc<str>,
    },
    CompletionVerificationStarted,
    CompletionVerificationEnded {
        outcome: CompletionVerificationOutcome,
        feedback: Option<Arc<str>>,
    },
    ContextCompactionStarted {
        strategy: Arc<str>,
        estimated_tokens: usize,
        target_tokens: usize,
    },
    ContextCompactionCompleted {
        strategy: Arc<str>,
        estimated_tokens_before: usize,
        estimated_tokens_after: usize,
        removed_messages: usize,
    },
    ContextCompactionFailed {
        strategy: Arc<str>,
        message: Arc<str>,
    },
    ReasoningStarted {
        response_index: i32,
    },
    ReasoningChunk {
        response_index: i32,
        delta: Arc<str>,
    },
    ReasoningEnded {
        response_index: i32,
        outcome: StreamEnd,
    },
    ContentStarted {
        response_index: i32,
    },
    ContentChunk {
        response_index: i32,
        delta: Arc<str>,
    },
    ContentEnded {
        response_index: i32,
        outcome: StreamEnd,
    },
    ToolCallStarted {
        key: ToolCallKey,
    },
    ToolCallChunk {
        key: ToolCallKey,
        id_delta: Option<Arc<str>>,
        name_delta: Option<Arc<str>>,
        arguments_delta: Option<Arc<str>>,
    },
    ToolCallEnded {
        key: ToolCallKey,
        outcome: StreamEnd,
        tool_call: Option<Arc<ModelToolCall>>,
    },
    ToolApprovalRequested {
        call_id: Arc<str>,
        name: Arc<str>,
        capability: ToolCapability,
    },
    ToolApprovalResolved {
        call_id: Arc<str>,
        approved: bool,
    },
    ToolExecutionStarted {
        call_id: Arc<str>,
        name: Arc<str>,
        arguments: Arc<str>,
    },
    ToolExecutionEnded {
        call_id: Arc<str>,
        name: Arc<str>,
        outcome: Arc<ToolExecutionOutcome>,
    },
    UsageUpdated {
        usage: Arc<ModelUsage>,
    },
}
