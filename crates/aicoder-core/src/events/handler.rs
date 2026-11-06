use std::{sync::Arc, time::Duration};

use crate::{
    model::{ModelToolCall, ModelUsage, StopReason},
    state::AgentStateTransition,
    tools::ToolCapability,
};

use super::{
    AgentRawEvent, AgentRawEventEnvelope, AgentStage, CompletionVerificationOutcome, RoundOutcome,
    RunId, StreamEnd, ToolCallKey, ToolExecutionOutcome,
};

/// Metadata shared by every typed event.
#[derive(Debug, Clone)]
pub struct AgentEventMeta {
    pub schema_version: u32,
    pub session_id: Option<Arc<str>>,
    pub turn_id: Option<Arc<str>>,
    pub elapsed_ms: u64,
    pub termination: Option<Arc<super::RunTermination>>,
    pub run_id: RunId,
    pub sequence: u64,
    pub round: Option<usize>,
}

impl From<&AgentRawEventEnvelope> for AgentEventMeta {
    fn from(envelope: &AgentRawEventEnvelope) -> Self {
        Self {
            schema_version: envelope.schema_version,
            session_id: envelope.session_id.clone(),
            turn_id: envelope.turn_id.clone(),
            elapsed_ms: envelope.elapsed_ms,
            termination: envelope.termination.clone(),
            run_id: envelope.run_id,
            sequence: envelope.sequence,
            round: envelope.round,
        }
    }
}

macro_rules! meta_event {
    ($name:ident) => {
        #[derive(Debug, Clone)]
        pub struct $name {
            pub meta: AgentEventMeta,
        }
    };
}

#[derive(Debug, Clone)]
pub struct AgentStartedEvent {
    pub meta: AgentEventMeta,
    pub model: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct AgentCompletedEvent {
    pub meta: AgentEventMeta,
    pub rounds: usize,
    pub usage: Arc<ModelUsage>,
}

#[derive(Debug, Clone)]
pub struct AgentFailedEvent {
    pub meta: AgentEventMeta,
    pub stage: AgentStage,
    pub message: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct AgentAbortedEvent {
    pub meta: AgentEventMeta,
    pub reason: Arc<str>,
}
#[derive(Debug, Clone)]
pub struct AgentStoppedEvent {
    pub meta: AgentEventMeta,
    pub stop: Arc<crate::RunStop>,
}

#[derive(Debug, Clone)]
pub struct AgentStateChangedEvent {
    pub meta: AgentEventMeta,
    pub transition: AgentStateTransition,
}

meta_event!(RoundStartedEvent);

#[derive(Debug, Clone)]
pub struct RoundCompletedEvent {
    pub meta: AgentEventMeta,
    pub outcome: RoundOutcome,
}

meta_event!(ModelRequestStartedEvent);
meta_event!(ModelResponseStartedEvent);

#[derive(Debug, Clone)]
pub struct ModelRetryScheduledEvent {
    pub meta: AgentEventMeta,
    pub attempt: u32,
    pub delay: Duration,
    pub reason: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct ModelResponseCompletedEvent {
    pub meta: AgentEventMeta,
    pub finish_reason: Option<StopReason>,
}

#[derive(Debug, Clone)]
pub struct ModelResponseFailedEvent {
    pub meta: AgentEventMeta,
    pub message: Arc<str>,
}

meta_event!(CompletionVerificationStartedEvent);

#[derive(Debug, Clone)]
pub struct CompletionVerificationEndedEvent {
    pub meta: AgentEventMeta,
    pub outcome: CompletionVerificationOutcome,
    pub feedback: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct ContextCompactionStartedEvent {
    pub meta: AgentEventMeta,
    pub strategy: Arc<str>,
    pub estimated_tokens: usize,
    pub target_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct ContextCompactionCompletedEvent {
    pub meta: AgentEventMeta,
    pub strategy: Arc<str>,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
    pub removed_messages: usize,
}

#[derive(Debug, Clone)]
pub struct ContextCompactionFailedEvent {
    pub meta: AgentEventMeta,
    pub strategy: Arc<str>,
    pub message: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct ReasoningStartedEvent {
    pub meta: AgentEventMeta,
    pub response_index: i32,
}

#[derive(Debug, Clone)]
pub struct ReasoningChunkEvent {
    pub meta: AgentEventMeta,
    pub response_index: i32,
    pub delta: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct ReasoningEndedEvent {
    pub meta: AgentEventMeta,
    pub response_index: i32,
    pub outcome: StreamEnd,
}

#[derive(Debug, Clone)]
pub struct ContentStartedEvent {
    pub meta: AgentEventMeta,
    pub response_index: i32,
}

#[derive(Debug, Clone)]
pub struct ContentChunkEvent {
    pub meta: AgentEventMeta,
    pub response_index: i32,
    pub delta: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct ContentEndedEvent {
    pub meta: AgentEventMeta,
    pub response_index: i32,
    pub outcome: StreamEnd,
}

#[derive(Debug, Clone)]
pub struct ToolCallStartedEvent {
    pub meta: AgentEventMeta,
    pub key: ToolCallKey,
}

#[derive(Debug, Clone)]
pub struct ToolCallChunkEvent {
    pub meta: AgentEventMeta,
    pub key: ToolCallKey,
    pub id_delta: Option<Arc<str>>,
    pub name_delta: Option<Arc<str>>,
    pub arguments_delta: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct ToolCallEndedEvent {
    pub meta: AgentEventMeta,
    pub key: ToolCallKey,
    pub outcome: StreamEnd,
    pub tool_call: Option<Arc<ModelToolCall>>,
}

#[derive(Debug, Clone)]
pub struct ToolApprovalRequestedEvent {
    pub meta: AgentEventMeta,
    pub call_id: Arc<str>,
    pub name: Arc<str>,
    pub capability: ToolCapability,
    pub preview: Option<Arc<crate::tools::ToolPreview>>,
    pub session: Option<Arc<crate::tools::ApprovalSessionInfo>>,
}

#[derive(Debug, Clone)]
pub struct ToolApprovalResolvedEvent {
    pub meta: AgentEventMeta,
    pub call_id: Arc<str>,
    pub approved: bool,
    pub resolution: Option<Arc<crate::tools::ApprovalResolution>>,
}

#[derive(Debug, Clone)]
pub struct ToolExecutionStartedEvent {
    pub meta: AgentEventMeta,
    pub call_id: Arc<str>,
    pub name: Arc<str>,
    pub arguments: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct ToolExecutionEndedEvent {
    pub artifacts: Vec<crate::artifacts::ArtifactRef>,
    pub meta: AgentEventMeta,
    pub call_id: Arc<str>,
    pub name: Arc<str>,
    pub outcome: Arc<ToolExecutionOutcome>,
}

#[derive(Debug, Clone)]
pub struct LifecycleBoundaryCommittedEvent {
    pub meta: AgentEventMeta,
    pub boundary: crate::extensions::BoundaryKind,
    pub entries: usize,
    pub messages: usize,
    pub continue_run: bool,
}

#[derive(Debug, Clone)]
pub struct UsageUpdatedEvent {
    pub meta: AgentEventMeta,
    pub usage: Arc<ModelUsage>,
}

#[derive(Debug, Clone)]
pub struct SessionBoundEvent {
    pub meta: AgentEventMeta,
    pub session_id: Arc<str>,
    pub turn_id: Arc<str>,
}
#[derive(Debug, Clone)]
pub struct RunApprovalRequestedEvent {
    pub meta: AgentEventMeta,
    pub request: Arc<crate::PendingRunApproval>,
}
#[derive(Debug, Clone)]
pub struct RunInputAppliedEvent {
    pub meta: AgentEventMeta,
    pub input: Arc<crate::RunInput>,
}

#[derive(Debug, Clone)]
pub struct MessageFinalizedEvent {
    pub meta: AgentEventMeta,
    pub message: Arc<crate::model::Message>,
    pub custom: Option<crate::CustomMessageMetadata>,
    pub origin: crate::extensions::MessageOrigin,
    pub changed: bool,
}
#[derive(Debug, Clone)]
pub struct AssistantMessageFinalizedEvent {
    pub meta: AgentEventMeta,
    pub message: Arc<crate::model::Message>,
    pub changed: bool,
}
/// The single event interface for agent applications.
///
/// `on_raw_event` observes every event before its type-specific callback. Applications can use it
/// to forward or persist the event stream while overriding only the typed callbacks they need for
/// presentation. Payloads are owned and backed by `Arc` where useful, so callbacks can retain or
/// forward events without copying complete streaming chunks.
pub trait AgentEventHandler: Send + Sync + 'static {
    /// Explicit no-observation handler; avoids a callback worker and queue.
    fn is_noop(&self) -> bool {
        false
    }
    fn on_session_bound(&self, _event: SessionBoundEvent) {}
    fn on_raw_event(&self, _event: &AgentRawEventEnvelope) {}

    fn on_run_approval_requested(&self, _event: RunApprovalRequestedEvent) {}
    fn on_run_input_applied(&self, _event: RunInputAppliedEvent) {}
    fn on_message_finalized(&self, _event: MessageFinalizedEvent) {}
    fn on_assistant_message_finalized(&self, _event: AssistantMessageFinalizedEvent) {}
    fn on_agent_started(&self, _event: AgentStartedEvent) {}
    fn on_agent_completed(&self, _event: AgentCompletedEvent) {}
    fn on_agent_failed(&self, _event: AgentFailedEvent) {}
    fn on_agent_aborted(&self, _event: AgentAbortedEvent) {}
    fn on_agent_stopped(&self, _event: AgentStoppedEvent) {}
    fn on_agent_state_changed(&self, _event: AgentStateChangedEvent) {}
    fn on_round_started(&self, _event: RoundStartedEvent) {}
    fn on_round_completed(&self, _event: RoundCompletedEvent) {}

    fn on_model_request_started(&self, _event: ModelRequestStartedEvent) {}
    fn on_model_response_started(&self, _event: ModelResponseStartedEvent) {}
    fn on_model_retry_scheduled(&self, _event: ModelRetryScheduledEvent) {}
    fn on_model_response_completed(&self, _event: ModelResponseCompletedEvent) {}
    fn on_model_response_failed(&self, _event: ModelResponseFailedEvent) {}
    fn on_completion_verification_started(&self, _event: CompletionVerificationStartedEvent) {}
    fn on_completion_verification_ended(&self, _event: CompletionVerificationEndedEvent) {}
    fn on_context_compaction_started(&self, _event: ContextCompactionStartedEvent) {}
    fn on_context_compaction_completed(&self, _event: ContextCompactionCompletedEvent) {}
    fn on_context_compaction_failed(&self, _event: ContextCompactionFailedEvent) {}

    fn on_reasoning_started(&self, _event: ReasoningStartedEvent) {}
    fn on_reasoning_chunk(&self, _event: ReasoningChunkEvent) {}
    fn on_reasoning_ended(&self, _event: ReasoningEndedEvent) {}

    fn on_content_started(&self, _event: ContentStartedEvent) {}
    fn on_content_chunk(&self, _event: ContentChunkEvent) {}
    fn on_content_ended(&self, _event: ContentEndedEvent) {}

    fn on_tool_call_started(&self, _event: ToolCallStartedEvent) {}
    fn on_tool_call_chunk(&self, _event: ToolCallChunkEvent) {}
    fn on_tool_call_ended(&self, _event: ToolCallEndedEvent) {}

    fn on_tool_approval_requested(&self, _event: ToolApprovalRequestedEvent) {}
    fn on_tool_approval_resolved(&self, _event: ToolApprovalResolvedEvent) {}
    fn on_tool_execution_started(&self, _event: ToolExecutionStartedEvent) {}
    fn on_tool_execution_ended(&self, _event: ToolExecutionEndedEvent) {}

    fn on_lifecycle_boundary_committed(&self, _event: LifecycleBoundaryCommittedEvent) {}
    fn on_usage_updated(&self, _event: UsageUpdatedEvent) {}
}

impl AgentEventHandler for () {
    fn is_noop(&self) -> bool {
        true
    }
}

impl<F> AgentEventHandler for F
where
    F: Fn(&AgentRawEventEnvelope) + Send + Sync + 'static,
{
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        self(event);
    }
}

pub(crate) fn dispatch_event(handler: &dyn AgentEventHandler, envelope: &AgentRawEventEnvelope) {
    handler.on_raw_event(envelope);
    let meta = AgentEventMeta::from(envelope);
    match &envelope.event {
        AgentRawEvent::MessageFinalized {
            message,
            custom,
            origin,
            changed,
        } => handler.on_message_finalized(MessageFinalizedEvent {
            meta,
            message: message.clone(),
            custom: custom.clone(),
            origin: *origin,
            changed: *changed,
        }),
        AgentRawEvent::AssistantMessageFinalized { message, changed } => handler
            .on_assistant_message_finalized(AssistantMessageFinalizedEvent {
                meta,
                message: message.clone(),
                changed: *changed,
            }),
        AgentRawEvent::AgentStarted { model } => handler.on_agent_started(AgentStartedEvent {
            meta,
            model: Arc::clone(model),
        }),
        AgentRawEvent::AgentCompleted { rounds, usage } => {
            handler.on_agent_completed(AgentCompletedEvent {
                meta,
                rounds: *rounds,
                usage: Arc::clone(usage),
            });
        }
        AgentRawEvent::AgentFailed { stage, message } => {
            handler.on_agent_failed(AgentFailedEvent {
                meta,
                stage: *stage,
                message: Arc::clone(message),
            });
        }
        AgentRawEvent::AgentStopped { stop } => handler.on_agent_stopped(AgentStoppedEvent {
            meta,
            stop: stop.clone(),
        }),
        AgentRawEvent::AgentAborted { reason } => {
            handler.on_agent_aborted(AgentAbortedEvent {
                meta,
                reason: Arc::clone(reason),
            });
        }
        AgentRawEvent::StateChanged { transition } => {
            handler.on_agent_state_changed(AgentStateChangedEvent {
                meta,
                transition: *transition,
            });
        }
        AgentRawEvent::RunApprovalRequested { request } => {
            handler.on_run_approval_requested(RunApprovalRequestedEvent {
                meta,
                request: request.clone(),
            })
        }
        AgentRawEvent::RunInputApplied { input } => {
            handler.on_run_input_applied(RunInputAppliedEvent {
                meta,
                input: input.clone(),
            })
        }
        AgentRawEvent::SessionBound {
            session_id,
            turn_id,
        } => handler.on_session_bound(SessionBoundEvent {
            meta,
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
        }),
        AgentRawEvent::RoundStarted => {
            handler.on_round_started(RoundStartedEvent { meta });
        }
        AgentRawEvent::RoundCompleted { outcome } => {
            handler.on_round_completed(RoundCompletedEvent {
                meta,
                outcome: outcome.clone(),
            })
        }
        AgentRawEvent::ModelRequestStarted => {
            handler.on_model_request_started(ModelRequestStartedEvent { meta })
        }
        AgentRawEvent::ModelResponseStarted => {
            handler.on_model_response_started(ModelResponseStartedEvent { meta })
        }
        AgentRawEvent::ModelRetryScheduled {
            attempt,
            delay,
            reason,
        } => handler.on_model_retry_scheduled(ModelRetryScheduledEvent {
            meta,
            attempt: *attempt,
            delay: *delay,
            reason: Arc::clone(reason),
        }),
        AgentRawEvent::ModelResponseCompleted { finish_reason } => {
            handler.on_model_response_completed(ModelResponseCompletedEvent {
                meta,
                finish_reason: finish_reason.clone(),
            });
        }
        AgentRawEvent::ModelResponseFailed { message } => {
            handler.on_model_response_failed(ModelResponseFailedEvent {
                meta,
                message: Arc::clone(message),
            })
        }
        AgentRawEvent::CompletionVerificationStarted => {
            handler.on_completion_verification_started(CompletionVerificationStartedEvent { meta })
        }
        AgentRawEvent::CompletionVerificationEnded { outcome, feedback } => handler
            .on_completion_verification_ended(CompletionVerificationEndedEvent {
                meta,
                outcome: *outcome,
                feedback: feedback.clone(),
            }),
        AgentRawEvent::ContextCompactionStarted {
            strategy,
            estimated_tokens,
            target_tokens,
        } => handler.on_context_compaction_started(ContextCompactionStartedEvent {
            meta,
            strategy: Arc::clone(strategy),
            estimated_tokens: *estimated_tokens,
            target_tokens: *target_tokens,
        }),
        AgentRawEvent::ContextCompactionCompleted {
            strategy,
            estimated_tokens_before,
            estimated_tokens_after,
            removed_messages,
        } => handler.on_context_compaction_completed(ContextCompactionCompletedEvent {
            meta,
            strategy: Arc::clone(strategy),
            estimated_tokens_before: *estimated_tokens_before,
            estimated_tokens_after: *estimated_tokens_after,
            removed_messages: *removed_messages,
        }),
        AgentRawEvent::ContextCompactionFailed { strategy, message } => handler
            .on_context_compaction_failed(ContextCompactionFailedEvent {
                meta,
                strategy: Arc::clone(strategy),
                message: Arc::clone(message),
            }),
        AgentRawEvent::ReasoningStarted { response_index } => {
            handler.on_reasoning_started(ReasoningStartedEvent {
                meta,
                response_index: *response_index,
            })
        }
        AgentRawEvent::ReasoningChunk {
            response_index,
            delta,
        } => handler.on_reasoning_chunk(ReasoningChunkEvent {
            meta,
            response_index: *response_index,
            delta: Arc::clone(delta),
        }),
        AgentRawEvent::ReasoningEnded {
            response_index,
            outcome,
        } => handler.on_reasoning_ended(ReasoningEndedEvent {
            meta,
            response_index: *response_index,
            outcome: outcome.clone(),
        }),
        AgentRawEvent::ContentStarted { response_index } => {
            handler.on_content_started(ContentStartedEvent {
                meta,
                response_index: *response_index,
            });
        }
        AgentRawEvent::ContentChunk {
            response_index,
            delta,
        } => handler.on_content_chunk(ContentChunkEvent {
            meta,
            response_index: *response_index,
            delta: Arc::clone(delta),
        }),
        AgentRawEvent::ContentEnded {
            response_index,
            outcome,
        } => handler.on_content_ended(ContentEndedEvent {
            meta,
            response_index: *response_index,
            outcome: outcome.clone(),
        }),
        AgentRawEvent::ToolCallStarted { key } => {
            handler.on_tool_call_started(ToolCallStartedEvent { meta, key: *key })
        }
        AgentRawEvent::ToolCallChunk {
            key,
            id_delta,
            name_delta,
            arguments_delta,
        } => handler.on_tool_call_chunk(ToolCallChunkEvent {
            meta,
            key: *key,
            id_delta: id_delta.clone(),
            name_delta: name_delta.clone(),
            arguments_delta: arguments_delta.clone(),
        }),
        AgentRawEvent::ToolCallEnded {
            key,
            outcome,
            tool_call,
        } => handler.on_tool_call_ended(ToolCallEndedEvent {
            meta,
            key: *key,
            outcome: outcome.clone(),
            tool_call: tool_call.clone(),
        }),
        AgentRawEvent::ToolApprovalRequested {
            call_id,
            name,
            capability,
            preview,
            session,
        } => handler.on_tool_approval_requested(ToolApprovalRequestedEvent {
            meta,
            call_id: Arc::clone(call_id),
            name: Arc::clone(name),
            capability: *capability,
            preview: preview.clone(),
            session: session.clone(),
        }),
        AgentRawEvent::ToolApprovalResolved {
            call_id,
            approved,
            resolution,
        } => {
            handler.on_tool_approval_resolved(ToolApprovalResolvedEvent {
                meta,
                call_id: Arc::clone(call_id),
                approved: *approved,
                resolution: resolution.clone(),
            });
        }
        AgentRawEvent::ToolExecutionStarted {
            call_id,
            name,
            arguments,
        } => handler.on_tool_execution_started(ToolExecutionStartedEvent {
            meta,
            call_id: Arc::clone(call_id),
            name: Arc::clone(name),
            arguments: Arc::clone(arguments),
        }),
        AgentRawEvent::ToolExecutionEnded {
            artifacts,
            call_id,
            name,
            outcome,
        } => handler.on_tool_execution_ended(ToolExecutionEndedEvent {
            artifacts: artifacts.clone(),
            meta,
            call_id: Arc::clone(call_id),
            name: Arc::clone(name),
            outcome: Arc::clone(outcome),
        }),
        AgentRawEvent::LifecycleBoundaryCommitted {
            boundary,
            entries,
            messages,
            continue_run,
        } => handler.on_lifecycle_boundary_committed(LifecycleBoundaryCommittedEvent {
            meta,
            boundary: *boundary,
            entries: *entries,
            messages: *messages,
            continue_run: *continue_run,
        }),
        AgentRawEvent::UsageUpdated { usage } => handler.on_usage_updated(UsageUpdatedEvent {
            meta,
            usage: Arc::clone(usage),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct RecordingHandler {
        received: Mutex<Vec<String>>,
        raw_sequences: Mutex<Vec<u64>>,
    }

    impl AgentEventHandler for RecordingHandler {
        fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
            self.raw_sequences.lock().unwrap().push(event.sequence);
        }

        fn on_round_started(&self, event: RoundStartedEvent) {
            self.received
                .lock()
                .unwrap()
                .push(format!("round:{}", event.meta.sequence));
        }

        fn on_agent_state_changed(&self, event: AgentStateChangedEvent) {
            self.received.lock().unwrap().push(format!(
                "state:{}:{}",
                event.meta.sequence,
                event.transition.current.name()
            ));
        }

        fn on_reasoning_chunk(&self, event: ReasoningChunkEvent) {
            self.received
                .lock()
                .unwrap()
                .push(format!("reasoning:{}:{}", event.meta.sequence, event.delta));
        }

        fn on_content_chunk(&self, event: ContentChunkEvent) {
            self.received
                .lock()
                .unwrap()
                .push(format!("content:{}:{}", event.meta.sequence, event.delta));
        }

        fn on_usage_updated(&self, event: UsageUpdatedEvent) {
            self.received.lock().unwrap().push(format!(
                "usage:{}:{}",
                event.meta.sequence,
                event
                    .usage
                    .total_tokens
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".into())
            ));
        }
    }

    fn envelope(sequence: u64, event: AgentRawEvent) -> AgentRawEventEnvelope {
        AgentRawEventEnvelope {
            schema_version: crate::events::EVENT_SCHEMA_VERSION,
            session_id: None,
            turn_id: None,
            elapsed_ms: 0,
            termination: None,
            run_id: RunId::new(),
            sequence,
            round: Some(1),
            event,
        }
    }

    #[test]
    fn handler_routes_raw_and_typed_events_without_reordering() {
        let handler = Arc::new(RecordingHandler::default());
        dispatch_event(handler.as_ref(), &envelope(1, AgentRawEvent::RoundStarted));
        dispatch_event(
            handler.as_ref(),
            &envelope(
                2,
                AgentRawEvent::ReasoningChunk {
                    response_index: 0,
                    delta: Arc::from("thinking"),
                },
            ),
        );
        dispatch_event(
            handler.as_ref(),
            &envelope(
                3,
                AgentRawEvent::ContentChunk {
                    response_index: 0,
                    delta: Arc::from("hello"),
                },
            ),
        );
        dispatch_event(
            handler.as_ref(),
            &envelope(
                4,
                AgentRawEvent::UsageUpdated {
                    usage: Arc::new(ModelUsage {
                        total_tokens: Some(12),
                        ..ModelUsage::default()
                    }),
                },
            ),
        );
        dispatch_event(
            handler.as_ref(),
            &envelope(
                5,
                AgentRawEvent::StateChanged {
                    transition: AgentStateTransition {
                        previous: crate::state::AgentRunState::Idle,
                        current: crate::state::AgentRunState::Preparing,
                    },
                },
            ),
        );

        assert_eq!(
            *handler.received.lock().unwrap(),
            vec![
                "round:1",
                "reasoning:2:thinking",
                "content:3:hello",
                "usage:4:12",
                "state:5:preparing"
            ]
        );
        assert_eq!(*handler.raw_sequences.lock().unwrap(), vec![1, 2, 3, 4, 5]);
    }
}
