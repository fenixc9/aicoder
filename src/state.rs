//! Explicit lifecycle state for one agent run.

use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRunState {
    Idle,
    Preparing,
    AwaitingModel {
        round: usize,
    },
    VerifyingCompletion {
        round: usize,
    },
    ExecutingTools {
        round: usize,
        count: usize,
    },
    /// Reserved for context compaction once the loop supports it.
    Compacting {
        round: usize,
    },
    Completed {
        rounds: usize,
    },
    Failed {
        round: Option<usize>,
    },
    /// Reserved terminal state for cooperative cancellation support.
    Aborted {
        round: Option<usize>,
    },
}

impl AgentRunState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Preparing => "preparing",
            Self::AwaitingModel { .. } => "awaiting_model",
            Self::VerifyingCompletion { .. } => "verifying_completion",
            Self::ExecutingTools { .. } => "executing_tools",
            Self::Compacting { .. } => "compacting",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
            Self::Aborted { .. } => "aborted",
        }
    }

    pub fn round(self) -> Option<usize> {
        match self {
            Self::AwaitingModel { round }
            | Self::VerifyingCompletion { round }
            | Self::ExecutingTools { round, .. }
            | Self::Compacting { round } => Some(round),
            Self::Completed { rounds } => Some(rounds),
            Self::Failed { round } | Self::Aborted { round } => round,
            Self::Idle | Self::Preparing => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Aborted { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentStateTransition {
    pub previous: AgentRunState,
    pub current: AgentRunState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidAgentStateTransition {
    pub previous: AgentRunState,
    pub requested: AgentRunState,
}

impl fmt::Display for InvalidAgentStateTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid agent state transition from {:?} to {:?}",
            self.previous, self.requested
        )
    }
}

impl Error for InvalidAgentStateTransition {}

#[derive(Debug, Clone)]
pub struct AgentRunStateMachine {
    state: AgentRunState,
}

impl AgentRunStateMachine {
    pub fn new() -> Self {
        Self {
            state: AgentRunState::Idle,
        }
    }

    pub fn state(&self) -> AgentRunState {
        self.state
    }

    pub fn transition(
        &mut self,
        requested: AgentRunState,
    ) -> Result<AgentStateTransition, InvalidAgentStateTransition> {
        if !valid_transition(self.state, requested) {
            return Err(InvalidAgentStateTransition {
                previous: self.state,
                requested,
            });
        }
        let transition = AgentStateTransition {
            previous: self.state,
            current: requested,
        };
        self.state = requested;
        Ok(transition)
    }
}

impl Default for AgentRunStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

fn valid_transition(previous: AgentRunState, current: AgentRunState) -> bool {
    use AgentRunState::*;

    match (previous, current) {
        (Idle, Preparing) => true,
        (Preparing, AwaitingModel { round: 1 }) => true,
        (Preparing, Failed { round: None } | Aborted { round: None }) => true,
        (AwaitingModel { round }, VerifyingCompletion { round: next }) => round == next,
        (AwaitingModel { round }, ExecutingTools { round: next, count }) => {
            round == next && count > 0
        }
        (AwaitingModel { round }, Compacting { round: next }) => round == next,
        (Compacting { round }, AwaitingModel { round: next }) => round == next,
        (VerifyingCompletion { round }, AwaitingModel { round: next })
        | (ExecutingTools { round, .. }, AwaitingModel { round: next }) => {
            round.checked_add(1) == Some(next)
        }
        (VerifyingCompletion { round }, Completed { rounds }) => round == rounds,
        (
            AwaitingModel { round }
            | VerifyingCompletion { round }
            | ExecutingTools { round, .. }
            | Compacting { round },
            Failed {
                round: Some(failed_round),
            }
            | Aborted {
                round: Some(failed_round),
            },
        ) => round == failed_round,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_complete_tool_and_verification_path() {
        let mut machine = AgentRunStateMachine::new();
        for state in [
            AgentRunState::Preparing,
            AgentRunState::AwaitingModel { round: 1 },
            AgentRunState::ExecutingTools { round: 1, count: 2 },
            AgentRunState::AwaitingModel { round: 2 },
            AgentRunState::VerifyingCompletion { round: 2 },
            AgentRunState::AwaitingModel { round: 3 },
            AgentRunState::VerifyingCompletion { round: 3 },
            AgentRunState::Completed { rounds: 3 },
        ] {
            machine.transition(state).unwrap();
        }
        assert_eq!(machine.state(), AgentRunState::Completed { rounds: 3 });
    }

    #[test]
    fn rejects_skipped_and_post_terminal_transitions() {
        let mut machine = AgentRunStateMachine::new();
        assert!(
            machine
                .transition(AgentRunState::ExecutingTools { round: 1, count: 1 })
                .is_err()
        );
        machine.transition(AgentRunState::Preparing).unwrap();
        machine
            .transition(AgentRunState::AwaitingModel { round: 1 })
            .unwrap();
        machine
            .transition(AgentRunState::VerifyingCompletion { round: 1 })
            .unwrap();
        machine
            .transition(AgentRunState::Completed { rounds: 1 })
            .unwrap();
        assert!(
            machine
                .transition(AgentRunState::AwaitingModel { round: 2 })
                .is_err()
        );
    }

    #[test]
    fn supports_reserved_compaction_and_abort_paths() {
        let mut machine = AgentRunStateMachine::new();
        machine.transition(AgentRunState::Preparing).unwrap();
        machine
            .transition(AgentRunState::AwaitingModel { round: 1 })
            .unwrap();
        machine
            .transition(AgentRunState::Compacting { round: 1 })
            .unwrap();
        machine
            .transition(AgentRunState::AwaitingModel { round: 1 })
            .unwrap();
        machine
            .transition(AgentRunState::Aborted { round: Some(1) })
            .unwrap();
        assert!(machine.state().is_terminal());
        assert_eq!(machine.state().name(), "aborted");
    }
}
