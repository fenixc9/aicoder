//! Ordered, non-blocking event delivery for agent runs.

mod emitter;
mod handler;
mod types;
pub use emitter::AgentEventEmitter;
#[allow(unused_imports)]
pub use handler::{
    AgentCompletedEvent, AgentEventHandler, AgentEventMeta, AgentFailedEvent, AgentStartedEvent,
    CompletionVerificationEndedEvent, CompletionVerificationStartedEvent, ContentChunkEvent,
    ContentEndedEvent, ContentStartedEvent, ModelRequestStartedEvent, ModelResponseCompletedEvent,
    ModelResponseFailedEvent, ModelResponseStartedEvent, ModelRetryScheduledEvent,
    ReasoningChunkEvent, ReasoningEndedEvent, ReasoningStartedEvent, RoundCompletedEvent,
    RoundStartedEvent, ToolApprovalRequestedEvent, ToolApprovalResolvedEvent, ToolCallChunkEvent,
    ToolCallEndedEvent, ToolCallStartedEvent, ToolExecutionEndedEvent, ToolExecutionStartedEvent,
    UsageUpdatedEvent,
};
pub use types::{
    AgentRawEvent, AgentRawEventEnvelope, AgentStage, CompletionVerificationOutcome, RoundOutcome,
    RunId, StreamEnd, ToolCallKey, ToolExecutionOutcome,
};

pub(crate) use types::emit_full_response_events;
