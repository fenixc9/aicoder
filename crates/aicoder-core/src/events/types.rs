use std::{fmt, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    model::{ModelToolCall, ModelUsage, StopReason},
    state::AgentStateTransition,
    tools::ToolCapability,
};

pub const EVENT_SCHEMA_VERSION: u32 = 1;
#[derive(Debug, Clone, Serialize)]
pub struct AgentRawEventEnvelope {
    pub schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Arc<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Arc<str>>,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination: Option<Arc<RunTermination>>,
    pub run_id: RunId,
    pub sequence: u64,
    pub round: Option<usize>,
    pub event: AgentRawEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStage {
    Turn,
    Model,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEnd {
    Completed,
    Aborted { reason: Arc<str> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoundOutcome {
    FinalAnswer,
    CompletionRejected,
    Stopped { stop: Arc<crate::RunStop> },
    ToolCalls { count: usize },
    Failed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionVerificationOutcome {
    Accepted,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ToolCallKey {
    pub response_index: i32,
    pub tool_index: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolExecutionOutcome {
    Succeeded { output: Value, truncated: bool },
    Failed { code: String, message: String },
    TimedOut,
    ApprovalDenied,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRawEvent {
    SessionBound {
        session_id: Arc<str>,
        turn_id: Arc<str>,
    },
    RunApprovalRequested {
        request: Arc<crate::PendingRunApproval>,
    },
    RunInputApplied {
        input: Arc<crate::RunInput>,
    },
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<Arc<crate::tools::ToolPreview>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<Arc<crate::tools::ApprovalSessionInfo>>,
    },
    ToolApprovalResolved {
        call_id: Arc<str>,
        approved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution: Option<Arc<crate::tools::ApprovalResolution>>,
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

/// Small authoritative terminal summary; transcript bodies stay in the run result/session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTermination {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context: Option<crate::ModelContextInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_context: Option<Value>,
    pub stop: Option<crate::RunStop>,
    pub persistence_error: Option<Box<crate::RunFailure>>,
    pub usage: ModelUsage,
    pub rounds: usize,
    pub tool_calls: usize,
    pub output_bytes: usize,
    pub tokens: crate::run::TokenAccounting,
    pub elapsed_ms: u64,
    pub pending_inputs: Vec<PendingInputRef>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInputRef {
    pub run_id: String,
    pub id: u64,
    pub kind: crate::RunInputKind,
}
impl From<&crate::RunOutcome> for RunTermination {
    fn from(outcome: &crate::RunOutcome) -> Self {
        let progress = outcome.progress();
        Self {
            model_context: progress.model_context.clone(),
            input_context: progress.input_context.clone(),
            stop: outcome.stop().cloned(),
            persistence_error: match outcome {
                crate::RunOutcome::Interrupted(turn) => turn.persistence_error.clone(),
                _ => None,
            },
            usage: progress.usage.clone(),
            rounds: progress.rounds,
            tool_calls: progress.tool_calls,
            output_bytes: progress.output_bytes,
            tokens: progress.tokens.clone(),
            elapsed_ms: crate::run::millis(progress.elapsed),
            pending_inputs: progress
                .pending_inputs
                .iter()
                .map(|input| PendingInputRef {
                    run_id: input.run_id.clone(),
                    id: input.id,
                    kind: input.kind,
                })
                .collect(),
        }
    }
}
impl AgentRawEventEnvelope {
    /// Construct an unbound envelope for external recorders/test fixtures.
    pub fn new(run_id: RunId, sequence: u64, round: Option<usize>, event: AgentRawEvent) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            session_id: None,
            turn_id: None,
            elapsed_ms: 0,
            termination: None,
            run_id,
            sequence,
            round,
            event,
        }
    }
}
impl<'de> Deserialize<'de> for AgentRawEventEnvelope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            schema_version: u32,
            run_id: RunId,
            sequence: u64,
            #[serde(default)]
            session_id: Option<Arc<str>>,
            #[serde(default)]
            turn_id: Option<Arc<str>>,
            elapsed_ms: u64,
            #[serde(default)]
            termination: Option<Arc<RunTermination>>,
            round: Option<usize>,
            event: AgentRawEvent,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.schema_version != EVENT_SCHEMA_VERSION {
            return Err(serde::de::Error::custom("unsupported event schema version"));
        }
        if wire.sequence == 0
            || wire.round == Some(0)
            || wire.turn_id.is_some() && wire.session_id.is_none()
        {
            return Err(serde::de::Error::custom(
                "invalid event identity or sequence",
            ));
        }
        Ok(Self {
            schema_version: wire.schema_version,
            run_id: wire.run_id,
            sequence: wire.sequence,
            session_id: wire.session_id,
            turn_id: wire.turn_id,
            elapsed_ms: wire.elapsed_ms,
            termination: wire.termination,
            round: wire.round,
            event: wire.event,
        })
    }
}
impl AgentRawEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::AgentCompleted { .. }
                | Self::AgentFailed { .. }
                | Self::AgentAborted { .. }
                | Self::AgentStopped { .. }
        )
    }
}
