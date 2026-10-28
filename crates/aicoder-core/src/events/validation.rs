//! Validate a complete run trace. Mid-run subscriptions intentionally provide only a suffix.
use super::{AgentRawEvent, AgentRawEventEnvelope, EVENT_SCHEMA_VERSION, RunId};
use crate::{AgentRunState, AgentRunStateMachine, RunStop};
use anyhow::{Result, ensure};
#[derive(Debug, Default)]
pub struct EventTraceValidator {
    run_id: Option<RunId>,
    next_sequence: u64,
    session: Option<(String, String)>,
    state: AgentRunStateMachine,
    open_round: Option<usize>,
    last_round: usize,
    terminal: bool,
    invalid: bool,
    last_elapsed_ms: u64,
}
impl EventTraceValidator {
    pub fn new() -> Self {
        Self {
            next_sequence: 1,
            ..Self::default()
        }
    }
    pub fn observe(&mut self, envelope: &AgentRawEventEnvelope) -> Result<()> {
        ensure!(!self.invalid, "event trace was already rejected");
        let result = self.observe_inner(envelope);
        if result.is_err() {
            self.invalid = true;
        }
        result
    }
    fn observe_inner(&mut self, envelope: &AgentRawEventEnvelope) -> Result<()> {
        ensure!(!self.terminal, "event after terminal event");
        ensure!(
            envelope.schema_version == EVENT_SCHEMA_VERSION,
            "unsupported event schema version"
        );
        let expected = if self.next_sequence == 0 {
            1
        } else {
            self.next_sequence
        };
        ensure!(
            envelope.sequence == expected,
            "non-contiguous event sequence"
        );
        ensure!(
            envelope.elapsed_ms >= self.last_elapsed_ms,
            "event elapsed time moved backwards"
        );
        if let Some(id) = self.run_id {
            ensure!(id == envelope.run_id, "run identity changed");
        } else {
            ensure!(
                matches!(envelope.event, AgentRawEvent::AgentStarted { .. }),
                "trace must begin with AgentStarted"
            );
            self.run_id = Some(envelope.run_id);
        }
        if let AgentRawEvent::SessionBound {
            session_id,
            turn_id,
        } = &envelope.event
        {
            ensure!(self.session.is_none(), "session bound more than once");
            ensure!(
                !session_id.is_empty() && !turn_id.is_empty(),
                "empty session identity"
            );
            self.session = Some((session_id.to_string(), turn_id.to_string()));
        }
        match &self.session {
            Some((session, turn)) => ensure!(
                envelope.session_id.as_deref() == Some(session.as_str())
                    && envelope.turn_id.as_deref() == Some(turn.as_str()),
                "session identity changed"
            ),
            None => ensure!(
                envelope.session_id.is_none() && envelope.turn_id.is_none(),
                "session metadata without SessionBound"
            ),
        }
        match &envelope.event {
            AgentRawEvent::AgentStarted { .. } => ensure!(expected == 1, "duplicate AgentStarted"),
            AgentRawEvent::StateChanged { transition } => {
                ensure!(
                    self.state.state() == transition.previous,
                    "state transition has the wrong previous state"
                );
                self.state.transition(transition.current)?;
            }
            AgentRawEvent::RoundStarted => {
                let round = envelope
                    .round
                    .ok_or_else(|| anyhow::anyhow!("round event is unbound"))?;
                ensure!(
                    self.open_round.is_none() && self.last_round.checked_add(1) == Some(round),
                    "round start is unpaired or out of order"
                );
                self.last_round = round;
                self.open_round = Some(round);
            }
            AgentRawEvent::RoundCompleted { .. } => {
                ensure!(
                    self.open_round.is_some() && self.open_round == envelope.round,
                    "round end is unpaired"
                );
                self.open_round = None;
            }
            event if event.is_terminal() => {
                ensure!(
                    self.open_round.is_none(),
                    "terminal event has an open round"
                );
                ensure!(
                    self.state.state().is_terminal(),
                    "terminal event is missing its terminal state"
                );
                let summary = envelope
                    .termination
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("terminal event is missing its summary"))?;
                ensure!(
                    summary.rounds == self.last_round,
                    "terminal round count differs from trace"
                );
                let state = self.state.state();
                match (event, &summary.stop) {
                    (AgentRawEvent::AgentCompleted { rounds, usage }, None) => ensure!(
                        matches!(state, AgentRunState::Completed { .. })
                            && *rounds == summary.rounds
                            && usage.as_ref() == &summary.usage,
                        "completion summary differs"
                    ),
                    (
                        AgentRawEvent::AgentAborted { reason },
                        Some(RunStop::Cancelled {
                            reason: summary_reason,
                        }),
                    ) => ensure!(
                        matches!(state, AgentRunState::Aborted { .. })
                            && reason.as_ref() == summary_reason,
                        "cancellation summary differs"
                    ),
                    (AgentRawEvent::AgentStopped { stop }, Some(summary_stop)) => ensure!(
                        matches!(state, AgentRunState::Stopped { .. })
                            && stop.as_ref() == summary_stop,
                        "stop summary differs"
                    ),
                    (AgentRawEvent::AgentFailed { .. }, Some(RunStop::Failed(_))) => ensure!(
                        matches!(state, AgentRunState::Failed { .. }),
                        "failure state differs"
                    ),
                    _ => anyhow::bail!("terminal event disagrees with its stop"),
                }
                self.terminal = true;
            }
            _ => {}
        }
        if !envelope.event.is_terminal() {
            ensure!(
                envelope.termination.is_none(),
                "terminal summary attached to a nonterminal event"
            );
        }
        self.next_sequence = expected
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("event sequence exhausted"))?;
        self.last_elapsed_ms = envelope.elapsed_ms;
        Ok(())
    }
    pub fn finish(&self) -> Result<()> {
        ensure!(
            !self.invalid && self.terminal && self.open_round.is_none(),
            "incomplete event trace"
        );
        Ok(())
    }
}
