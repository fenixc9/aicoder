use std::{fmt, sync::Arc, time::Duration};

use serde_json::Value;
use uuid::Uuid;

use crate::{
    state::AgentStateTransition,
    tools::ToolCapability,
    types::{ChatCompletionResponse, FinishReason, ToolCall, Usage},
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
    pub choice_index: i32,
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
        usage: Arc<Usage>,
    },
    AgentFailed {
        stage: AgentStage,
        message: Arc<str>,
    },
    AgentAborted {
        reason: Arc<str>,
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
        finish_reason: Option<FinishReason>,
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
        choice_index: i32,
    },
    ReasoningChunk {
        choice_index: i32,
        delta: Arc<str>,
    },
    ReasoningEnded {
        choice_index: i32,
        outcome: StreamEnd,
    },
    ContentStarted {
        choice_index: i32,
    },
    ContentChunk {
        choice_index: i32,
        delta: Arc<str>,
    },
    ContentEnded {
        choice_index: i32,
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
        tool_call: Option<Arc<ToolCall>>,
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
        usage: Arc<Usage>,
    },
}

pub(crate) fn emit_full_response_events(
    events: &super::AgentEventEmitter,
    response: &ChatCompletionResponse,
) {
    events.emit(AgentRawEvent::ModelResponseStarted);
    for choice in &response.choices {
        if let Some(reasoning) = &choice.message.reasoning
            && !reasoning.is_empty()
        {
            events.emit(AgentRawEvent::ReasoningStarted {
                choice_index: choice.index,
            });
            events.emit(AgentRawEvent::ReasoningChunk {
                choice_index: choice.index,
                delta: reasoning.clone().into(),
            });
            events.emit(AgentRawEvent::ReasoningEnded {
                choice_index: choice.index,
                outcome: StreamEnd::Completed,
            });
        }
        if let Some(content) = &choice.message.content
            && !content.is_empty()
        {
            events.emit(AgentRawEvent::ContentStarted {
                choice_index: choice.index,
            });
            events.emit(AgentRawEvent::ContentChunk {
                choice_index: choice.index,
                delta: content.clone().into(),
            });
            events.emit(AgentRawEvent::ContentEnded {
                choice_index: choice.index,
                outcome: StreamEnd::Completed,
            });
        }
        for (tool_index, tool_call) in choice
            .message
            .tool_calls
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let key = ToolCallKey {
                choice_index: choice.index,
                tool_index: tool_index as i32,
            };
            events.emit(AgentRawEvent::ToolCallStarted { key });
            events.emit(AgentRawEvent::ToolCallChunk {
                key,
                id_delta: Some(tool_call.id.clone().into()),
                name_delta: Some(tool_call.function.name.clone().into()),
                arguments_delta: Some(tool_call.function.arguments.clone().into()),
            });
            events.emit(AgentRawEvent::ToolCallEnded {
                key,
                outcome: StreamEnd::Completed,
                tool_call: Some(tool_call.clone().into()),
            });
        }
    }
    if let Some(usage) = &response.usage {
        events.emit(AgentRawEvent::UsageUpdated {
            usage: usage.clone().into(),
        });
    }
    events.emit(AgentRawEvent::ModelResponseCompleted {
        finish_reason: response
            .choices
            .first()
            .and_then(|choice| choice.finish_reason.clone()),
    });
}
