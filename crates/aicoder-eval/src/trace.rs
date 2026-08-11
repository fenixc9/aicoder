use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aicoder_core::events::{
    AgentEventHandler, AgentRawEvent, AgentRawEventEnvelope, CompletionVerificationOutcome,
    RoundOutcome, ToolExecutionOutcome,
};
use anyhow::{Context, Result};
use serde::Serialize;

use crate::TrajectorySummary;

#[derive(Debug, Clone)]
pub struct TimedAgentEvent {
    pub elapsed: Duration,
    pub envelope: AgentRawEventEnvelope,
}

#[derive(Debug, Clone, Default)]
pub struct AgentTrace {
    pub events: Vec<TimedAgentEvent>,
}

impl AgentTrace {
    pub fn summary(&self) -> TrajectorySummary {
        let mut summary = TrajectorySummary::default();
        for recorded in &self.events {
            match &recorded.envelope.event {
                AgentRawEvent::RoundStarted => summary.rounds += 1,
                AgentRawEvent::RoundCompleted {
                    outcome: RoundOutcome::FinalAnswer,
                } => summary.final_answer_rounds += 1,
                AgentRawEvent::StateChanged { transition } => {
                    summary.state_transitions += 1;
                    summary.final_state = Some(transition.current.name().to_string());
                }
                AgentRawEvent::ModelRequestStarted => summary.model_requests += 1,
                AgentRawEvent::ModelRetryScheduled { .. } => summary.model_retries += 1,
                AgentRawEvent::CompletionVerificationStarted => {
                    summary.completion_candidates += 1;
                }
                AgentRawEvent::CompletionVerificationEnded { outcome, .. } => match outcome {
                    CompletionVerificationOutcome::Rejected => {
                        summary.completion_rejections += 1;
                    }
                    CompletionVerificationOutcome::Failed => {
                        summary.completion_verifier_failures += 1;
                    }
                    CompletionVerificationOutcome::Accepted => {}
                },
                AgentRawEvent::ContextCompactionCompleted {
                    removed_messages, ..
                } => {
                    summary.context_compactions += 1;
                    summary.compacted_messages += removed_messages;
                }
                AgentRawEvent::ContextCompactionFailed { .. } => {
                    summary.context_compaction_failures += 1;
                }
                AgentRawEvent::ModelResponseCompleted { finish_reason } => {
                    summary.finish_reason = finish_reason.as_ref().map(|reason| {
                        serde_json::to_value(reason)
                            .ok()
                            .and_then(|value| value.as_str().map(str::to_owned))
                            .unwrap_or_else(|| format!("{reason:?}"))
                    });
                }
                AgentRawEvent::ToolExecutionStarted { .. } => summary.tool_calls += 1,
                AgentRawEvent::ToolExecutionEnded { outcome, .. } => match outcome.as_ref() {
                    ToolExecutionOutcome::Succeeded { truncated, .. } => {
                        summary.successful_tool_calls += 1;
                        if *truncated {
                            summary.truncated_tool_outputs += 1;
                        }
                    }
                    ToolExecutionOutcome::Failed { .. } => summary.failed_tool_calls += 1,
                    ToolExecutionOutcome::TimedOut => {
                        summary.failed_tool_calls += 1;
                        summary.timed_out_tool_calls += 1;
                    }
                    ToolExecutionOutcome::ApprovalDenied => {
                        summary.failed_tool_calls += 1;
                        summary.approval_denials += 1;
                    }
                },
                AgentRawEvent::AgentFailed { .. } => summary.agent_failures += 1,
                _ => {}
            }
        }
        summary
    }

    /// Writes the complete ordered raw event stream as a portable JSON artifact.
    pub fn write_json(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let events = self
            .events
            .iter()
            .map(PortableTraceEvent::from)
            .collect::<Vec<_>>();
        let encoded = serde_json::to_string_pretty(&events)
            .context("Failed to encode evaluation event trace")?;
        fs::write(path, encoded)
            .with_context(|| format!("Failed to write event trace {}", path.display()))
    }
}

#[derive(Serialize)]
struct PortableTraceEvent {
    elapsed_ms: u64,
    run_id: String,
    sequence: u64,
    round: Option<usize>,
    kind: &'static str,
    event: String,
}

impl From<&TimedAgentEvent> for PortableTraceEvent {
    fn from(recorded: &TimedAgentEvent) -> Self {
        Self {
            elapsed_ms: recorded.elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            run_id: recorded.envelope.run_id.to_string(),
            sequence: recorded.envelope.sequence,
            round: recorded.envelope.round,
            kind: event_kind(&recorded.envelope.event),
            event: format!("{:?}", recorded.envelope.event),
        }
    }
}

fn event_kind(event: &AgentRawEvent) -> &'static str {
    match event {
        AgentRawEvent::AgentStarted { .. } => "agent_started",
        AgentRawEvent::AgentCompleted { .. } => "agent_completed",
        AgentRawEvent::AgentFailed { .. } => "agent_failed",
        AgentRawEvent::StateChanged { .. } => "state_changed",
        AgentRawEvent::RoundStarted => "round_started",
        AgentRawEvent::RoundCompleted { .. } => "round_completed",
        AgentRawEvent::ModelRequestStarted => "model_request_started",
        AgentRawEvent::ModelResponseStarted => "model_response_started",
        AgentRawEvent::ModelRetryScheduled { .. } => "model_retry_scheduled",
        AgentRawEvent::ModelResponseCompleted { .. } => "model_response_completed",
        AgentRawEvent::ModelResponseFailed { .. } => "model_response_failed",
        AgentRawEvent::CompletionVerificationStarted => "completion_verification_started",
        AgentRawEvent::CompletionVerificationEnded { .. } => "completion_verification_ended",
        AgentRawEvent::ContextCompactionStarted { .. } => "context_compaction_started",
        AgentRawEvent::ContextCompactionCompleted { .. } => "context_compaction_completed",
        AgentRawEvent::ContextCompactionFailed { .. } => "context_compaction_failed",
        AgentRawEvent::ReasoningStarted { .. } => "reasoning_started",
        AgentRawEvent::ReasoningChunk { .. } => "reasoning_chunk",
        AgentRawEvent::ReasoningEnded { .. } => "reasoning_ended",
        AgentRawEvent::ContentStarted { .. } => "content_started",
        AgentRawEvent::ContentChunk { .. } => "content_chunk",
        AgentRawEvent::ContentEnded { .. } => "content_ended",
        AgentRawEvent::ToolCallStarted { .. } => "tool_call_started",
        AgentRawEvent::ToolCallChunk { .. } => "tool_call_chunk",
        AgentRawEvent::ToolCallEnded { .. } => "tool_call_ended",
        AgentRawEvent::ToolApprovalRequested { .. } => "tool_approval_requested",
        AgentRawEvent::ToolApprovalResolved { .. } => "tool_approval_resolved",
        AgentRawEvent::ToolExecutionStarted { .. } => "tool_execution_started",
        AgentRawEvent::ToolExecutionEnded { .. } => "tool_execution_ended",
        AgentRawEvent::UsageUpdated { .. } => "usage_updated",
    }
}

/// Thread-safe raw event handler used by the evaluation runner.
pub struct AgentTraceRecorder {
    started_at: Instant,
    events: Mutex<Vec<TimedAgentEvent>>,
}

impl AgentTraceRecorder {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            events: Mutex::new(Vec::new()),
        }
    }

    pub fn trace(&self) -> AgentTrace {
        AgentTrace {
            events: self
                .events
                .lock()
                .expect("agent trace recorder lock poisoned")
                .clone(),
        }
    }

    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }
}

impl Default for AgentTraceRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentEventHandler for AgentTraceRecorder {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        self.events
            .lock()
            .expect("agent trace recorder lock poisoned")
            .push(TimedAgentEvent {
                elapsed: self.started_at.elapsed(),
                envelope: event.clone(),
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicoder_core::events::RunId;

    #[test]
    fn summary_counts_context_compaction_activity() {
        let run_id = RunId::new();
        let trace = AgentTrace {
            events: vec![
                TimedAgentEvent {
                    elapsed: Duration::ZERO,
                    envelope: AgentRawEventEnvelope {
                        run_id,
                        sequence: 1,
                        round: Some(1),
                        event: AgentRawEvent::ContextCompactionCompleted {
                            strategy: "test".into(),
                            estimated_tokens_before: 100,
                            estimated_tokens_after: 50,
                            removed_messages: 3,
                        },
                    },
                },
                TimedAgentEvent {
                    elapsed: Duration::ZERO,
                    envelope: AgentRawEventEnvelope {
                        run_id,
                        sequence: 2,
                        round: Some(2),
                        event: AgentRawEvent::ContextCompactionFailed {
                            strategy: "test".into(),
                            message: "failed".into(),
                        },
                    },
                },
            ],
        };

        let summary = trace.summary();
        assert_eq!(summary.context_compactions, 1);
        assert_eq!(summary.context_compaction_failures, 1);
        assert_eq!(summary.compacted_messages, 3);
    }
}
