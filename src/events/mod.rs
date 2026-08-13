//! Ordered, non-blocking event delivery for agent runs.

mod emitter;
mod handler;
mod types;
pub use emitter::AgentEventEmitter;
pub(crate) use handler::dispatch_event;
#[allow(unused_imports)]
pub use handler::{
    AgentAbortedEvent, AgentCompletedEvent, AgentEventHandler, AgentEventMeta, AgentFailedEvent,
    AgentStartedEvent, AgentStateChangedEvent, CompletionVerificationEndedEvent,
    CompletionVerificationStartedEvent, ContentChunkEvent, ContentEndedEvent, ContentStartedEvent,
    ContextCompactionCompletedEvent, ContextCompactionFailedEvent, ContextCompactionStartedEvent,
    ModelRequestStartedEvent, ModelResponseCompletedEvent, ModelResponseFailedEvent,
    ModelResponseStartedEvent, ModelRetryScheduledEvent, ReasoningChunkEvent, ReasoningEndedEvent,
    ReasoningStartedEvent, RoundCompletedEvent, RoundStartedEvent, ToolApprovalRequestedEvent,
    ToolApprovalResolvedEvent, ToolCallChunkEvent, ToolCallEndedEvent, ToolCallStartedEvent,
    ToolExecutionEndedEvent, ToolExecutionStartedEvent, UsageUpdatedEvent,
};
pub use types::{
    AgentRawEvent, AgentRawEventEnvelope, AgentStage, CompletionVerificationOutcome, RoundOutcome,
    RunId, StreamEnd, ToolCallKey, ToolExecutionOutcome,
};

pub(crate) use types::emit_full_response_events;
