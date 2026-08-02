//! Ordered, non-blocking event delivery for agent runs.

mod emitter;
mod handler;
mod types;
pub use emitter::{AgentRawEventHandler, NoopRawEventHandler};
#[allow(unused_imports)]
pub use handler::{
    AgentCompletedEvent, AgentEventMeta, AgentFailedEvent, AgentStartedEvent,
    AgentTypeEventHandler, ContentChunkEvent, ContentEndedEvent, ContentStartedEvent,
    ModelRequestStartedEvent, ModelResponseCompletedEvent, ModelResponseFailedEvent,
    ModelResponseStartedEvent, ModelRetryScheduledEvent, ReasoningChunkEvent, ReasoningEndedEvent,
    ReasoningStartedEvent, RoundCompletedEvent, RoundStartedEvent, ToolApprovalRequestedEvent,
    ToolApprovalResolvedEvent, ToolCallChunkEvent, ToolCallEndedEvent, ToolCallStartedEvent,
    ToolExecutionEndedEvent, ToolExecutionStartedEvent, UsageUpdatedEvent,
};
pub use types::{
    AgentRawEvent, AgentRawEventEnvelope, AgentStage, RoundOutcome, RunId, StreamEnd, ToolCallKey,
    ToolExecutionOutcome,
};

pub(crate) use emitter::AgentEventSink;
pub(crate) use handler::AgentTypeEventAdapter;
pub(crate) use types::emit_full_response_events;
#[allow(unused_imports)]
pub(crate) use types::{AgentRawEvent as AgentEvent, AgentRawEventEnvelope as AgentEventEnvelope};
