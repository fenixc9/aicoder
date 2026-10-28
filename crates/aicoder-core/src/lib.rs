//! Reusable core for building coding-agent frontends.
//!
//! The crate owns the model/tool loop and exposes its provider, event, tool, and model types so a
//! CLI, TUI, server, or embedded application can choose its own configuration and presentation.

pub mod agent;
pub mod cancellation;
pub mod completion;
pub mod context;
pub mod events;
pub mod model;
pub mod redaction;
pub mod run;
pub mod session;
pub mod state;
pub mod tools;
pub mod turn_executor;

pub use agent::{
    Agent, AgentConfig, AgentTurnOutcome, AgentTurnResult, InterruptedAgentTurn, SessionSelection,
};
pub use cancellation::{TurnCancelled, TurnExecutionContext};
pub use completion::{
    AcceptAllCompletionVerifier, CompletionContext, CompletionVerdict, CompletionVerifier,
    WorkspaceChangeVerifier,
};
pub use context::{
    ContextCompaction, ContextCompactionInput, ContextCompactor, ContextWindowConfig,
    PruningContextCompactor, estimate_context_tokens,
};
pub use events::{
    AgentEventHandler, AgentRawEvent, AgentRawEventEnvelope, CompletionVerificationOutcome, RunId,
    ToolExecutionOutcome,
};
pub use state::{
    AgentRunState, AgentRunStateMachine, AgentStateTransition, InvalidAgentStateTransition,
};
pub use turn_executor::{TurnExecutionConfig, TurnExecutor, TurnExecutorBuilder};

pub use model::ModelProvider;
pub use run::{
    BudgetExceeded, BudgetResource, InterruptedTurn, RunBudget, RunFailure, RunOutcome,
    RunProgress, RunStage, RunStop, TurnExecutionResult,
};
