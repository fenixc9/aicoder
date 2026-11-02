use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aicoder_core::events::{
    AgentEventHandler, AgentRawEvent, AgentRawEventEnvelope, CompletionVerificationOutcome,
    RoundOutcome, ToolExecutionOutcome,
};
use anyhow::{Context, Result};

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
    /// Read and validate an archived event stream; never executes a model or tool.
    pub fn read_json(path: impl AsRef<Path>, limits: TraceLimits) -> Result<Self> {
        let limits = limits.validate()?;
        let bytes = crate::bounds::read(path.as_ref(), limits.max_bytes)?;
        let trace = Self::decode_json(&bytes, limits)?;
        trace.validate()?;
        Ok(trace)
    }
    pub(crate) fn decode_json(bytes: &[u8], limits: TraceLimits) -> Result<Self> {
        use serde::de::{DeserializeSeed, SeqAccess, Visitor};
        let limits = limits.validate()?;
        anyhow::ensure!(
            bytes.len() <= limits.max_bytes,
            "trace replay byte budget exhausted"
        );
        struct Events(usize);
        impl<'de> Visitor<'de> for Events {
            type Value = Vec<AgentRawEventEnvelope>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded event array")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut events = Vec::new();
                while let Some(event) = seq.next_element()? {
                    if events.len() >= self.0 {
                        return Err(serde::de::Error::custom("event replay count limit"));
                    }
                    events.push(event);
                }
                Ok(events)
            }
        }
        impl<'de> DeserializeSeed<'de> for Events {
            type Value = Vec<AgentRawEventEnvelope>;
            fn deserialize<D: serde::Deserializer<'de>>(
                self,
                d: D,
            ) -> std::result::Result<Self::Value, D::Error> {
                d.deserialize_seq(self)
            }
        }
        let mut decoder = serde_json::Deserializer::from_slice(bytes);
        let events = Events(limits.max_events).deserialize(&mut decoder)?;
        decoder.end()?;
        let trace = Self {
            events: events
                .into_iter()
                .map(|envelope| TimedAgentEvent {
                    elapsed: Duration::from_millis(envelope.elapsed_ms),
                    envelope,
                })
                .collect(),
        };
        Ok(trace)
    }
    pub fn validate(&self) -> Result<()> {
        let mut validator = aicoder_core::events::EventTraceValidator::new();
        for record in &self.events {
            validator.observe(&record.envelope)?;
        }
        validator.finish()
    }
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
                AgentRawEvent::AgentStopped { .. } => summary.agent_stops += 1,
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
                    ToolExecutionOutcome::Cancelled => {}
                },
                AgentRawEvent::AgentFailed { .. } => summary.agent_failures += 1,
                AgentRawEvent::AgentAborted { .. } => summary.agent_aborts += 1,
                _ => {}
            }
        }
        summary
    }

    /// Writes the complete ordered raw event stream as a portable JSON artifact.
    pub fn write_json(&self, path: impl AsRef<Path>) -> Result<()> {
        use std::io::Write;
        let path = path.as_ref();
        let limits = TraceLimits::default();
        anyhow::ensure!(
            self.events.len() <= limits.max_events,
            "trace serialization event budget exhausted"
        );
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        let events = self.events.iter().map(|e| &e.envelope).collect::<Vec<_>>();
        let mut writer = crate::bounds::LimitedWriter {
            inner: temporary.as_file_mut(),
            remaining: limits.max_bytes,
        };
        serde_json::to_writer(&mut writer, &events)
            .context("Failed to encode bounded event trace")?;
        writer.flush()?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("Failed to publish event trace {}", path.display()))?;
        Ok(())
    }
}

/// Explicit prefix retention bounds. After a gap no later event is retained as contiguous.
#[derive(Debug, Clone, Copy)]
pub struct TraceLimits {
    pub max_events: usize,
    pub max_bytes: usize,
}
impl Default for TraceLimits {
    fn default() -> Self {
        Self {
            max_events: 100_000,
            max_bytes: 32 * 1024 * 1024,
        }
    }
}
impl TraceLimits {
    pub(crate) fn validate(self) -> Result<Self> {
        anyhow::ensure!(
            (1..=1_000_000).contains(&self.max_events)
                && (1..=128 * 1024 * 1024).contains(&self.max_bytes),
            "invalid evaluation trace limits"
        );
        Ok(self)
    }
}
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TraceCapture {
    pub retained_events: usize,
    pub retained_bytes: usize,
    pub dropped_events: u64,
    pub first_missing_sequence: Option<u64>,
}
impl TraceCapture {
    pub fn complete(&self) -> bool {
        self.first_missing_sequence.is_none()
    }
}
#[derive(Default)]
struct Recorded {
    events: Vec<TimedAgentEvent>,
    capture: TraceCapture,
}
/// Thread-safe bounded raw event handler used by the evaluation runner.
pub struct AgentTraceRecorder {
    started_at: Instant,
    limits: TraceLimits,
    recorded: Mutex<Recorded>,
}
impl AgentTraceRecorder {
    pub fn new() -> Self {
        Self::with_limits(TraceLimits::default()).expect("default trace limits")
    }
    pub fn with_limits(limits: TraceLimits) -> Result<Self> {
        Ok(Self {
            started_at: Instant::now(),
            limits: limits.validate()?,
            recorded: Mutex::new(Recorded::default()),
        })
    }
    pub fn trace(&self) -> AgentTrace {
        AgentTrace {
            events: self.recorded.lock().expect("trace poisoned").events.clone(),
        }
    }
    pub fn capture_status(&self) -> TraceCapture {
        self.recorded
            .lock()
            .expect("trace poisoned")
            .capture
            .clone()
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
        let mut recorded = self.recorded.lock().expect("trace poisoned");
        let size = if recorded.capture.complete() && recorded.events.len() < self.limits.max_events
        {
            crate::bounds::encoded_size(
                event,
                self.limits
                    .max_bytes
                    .saturating_sub(recorded.capture.retained_bytes)
                    .saturating_sub(recorded.capture.retained_events + 2),
            )
            .ok()
        } else {
            None
        };
        if let Some(size) = size {
            recorded.capture.retained_bytes += size;
            recorded.capture.retained_events += 1;
            recorded.events.push(TimedAgentEvent {
                elapsed: self.started_at.elapsed(),
                envelope: event.clone(),
            });
        } else {
            recorded
                .capture
                .first_missing_sequence
                .get_or_insert(event.sequence);
            recorded.capture.dropped_events = recorded.capture.dropped_events.saturating_add(1);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use aicoder_core::events::RunId;

    #[test]
    fn exported_trace_contains_the_canonical_event_object_instead_of_debug_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace.json");
        let trace = AgentTrace {
            events: vec![TimedAgentEvent {
                elapsed: Duration::ZERO,
                envelope: AgentRawEventEnvelope::new(
                    RunId::new(),
                    1,
                    None,
                    AgentRawEvent::AgentStarted {
                        model: "offline".into(),
                    },
                ),
            }],
        };
        trace.write_json(&path).unwrap();
        let records: Vec<AgentRawEventEnvelope> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(records.len(), 1);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            value[0]["schema_version"],
            aicoder_core::events::EVENT_SCHEMA_VERSION
        );
        assert_eq!(value[0]["event"]["type"], "agent_started");
        assert!(value[0]["event"].is_object());
    }

    #[test]
    fn trace_byte_admission_reserves_array_framing() {
        let event = AgentRawEventEnvelope::new(
            RunId::new(),
            1,
            None,
            AgentRawEvent::AgentStarted {
                model: "offline".into(),
            },
        );
        let size = crate::bounds::encoded_size(&event, 4096).unwrap();
        let recorder = AgentTraceRecorder::with_limits(TraceLimits {
            max_events: 2,
            max_bytes: size + 2,
        })
        .unwrap();
        recorder.on_raw_event(&event);
        assert_eq!(recorder.capture_status().retained_events, 1);
        let trace = recorder.trace();
        let events = trace.events.iter().map(|e| &e.envelope).collect::<Vec<_>>();
        assert_eq!(
            crate::bounds::encode(&events, size + 2).unwrap().len(),
            size + 2
        );
        let recorder = AgentTraceRecorder::with_limits(TraceLimits {
            max_events: 2,
            max_bytes: size + 1,
        })
        .unwrap();
        recorder.on_raw_event(&event);
        assert_eq!(recorder.capture_status().retained_events, 0);
        assert_eq!(recorder.capture_status().first_missing_sequence, Some(1));
    }
    #[test]
    fn oversized_trace_publication_preserves_the_previous_file_and_discards_staging() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace.json");
        std::fs::write(&path, b"old").unwrap();
        let trace = AgentTrace {
            events: vec![TimedAgentEvent {
                elapsed: Duration::ZERO,
                envelope: AgentRawEventEnvelope::new(
                    RunId::new(),
                    1,
                    None,
                    AgentRawEvent::ContentChunk {
                        response_index: 0,
                        delta: "x".repeat(32 * 1024 * 1024).into(),
                    },
                ),
            }],
        };
        assert!(trace.write_json(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn summary_counts_context_compaction_activity() {
        let run_id = RunId::new();
        let trace = AgentTrace {
            events: vec![
                TimedAgentEvent {
                    elapsed: Duration::ZERO,
                    envelope: AgentRawEventEnvelope {
                        schema_version: aicoder_core::events::EVENT_SCHEMA_VERSION,
                        session_id: None,
                        turn_id: None,
                        elapsed_ms: 0,
                        termination: None,
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
                        schema_version: aicoder_core::events::EVENT_SCHEMA_VERSION,
                        session_id: None,
                        turn_id: None,
                        elapsed_ms: 0,
                        termination: None,
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
