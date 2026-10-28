//! Ordered, non-blocking event delivery for agent runs.

mod emitter;
mod handler;
mod model;
mod types;
pub use emitter::AgentEventEmitter;
#[allow(unused_imports)]
pub use handler::{
    AgentAbortedEvent, AgentCompletedEvent, AgentEventHandler, AgentEventMeta, AgentFailedEvent,
    AgentStartedEvent, AgentStateChangedEvent, AgentStoppedEvent, CompletionVerificationEndedEvent,
    CompletionVerificationStartedEvent, ContentChunkEvent, ContentEndedEvent, ContentStartedEvent,
    ContextCompactionCompletedEvent, ContextCompactionFailedEvent, ContextCompactionStartedEvent,
    ModelRequestStartedEvent, ModelResponseCompletedEvent, ModelResponseFailedEvent,
    ModelResponseStartedEvent, ModelRetryScheduledEvent, ReasoningChunkEvent, ReasoningEndedEvent,
    ReasoningStartedEvent, RoundCompletedEvent, RoundStartedEvent, RunApprovalRequestedEvent,
    RunInputAppliedEvent, ToolApprovalRequestedEvent, ToolApprovalResolvedEvent,
    ToolCallChunkEvent, ToolCallEndedEvent, ToolCallStartedEvent, ToolExecutionEndedEvent,
    ToolExecutionStartedEvent, UsageUpdatedEvent,
};
pub(crate) use model::RunModelEventSink;
pub use types::{
    AgentRawEvent, AgentRawEventEnvelope, AgentStage, CompletionVerificationOutcome, RoundOutcome,
    RunId, StreamEnd, ToolCallKey, ToolExecutionOutcome,
};
