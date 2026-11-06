//! Reusable core for building coding-agent frontends.
//!
//! The crate owns the model/tool loop and exposes its provider, event, tool, and model types so a
//! CLI, TUI, server, or embedded application can choose its own configuration and presentation.

pub mod agent;
mod custom_inbox;
pub use custom_inbox::{CustomInbox, PendingCustomMessage};
mod agent_prompt;
pub use agent_prompt::{
    AgentPrompt, CUSTOM_MESSAGE_NAMESPACE, CustomMessage, CustomMessageMetadata,
    MAX_CUSTOM_MESSAGE_BYTES, RunTrigger,
};
pub mod agent_input;
pub use agent_input::{AgentInput, InputSource};
pub mod cancellation;
pub mod completion;
pub mod composition;
pub mod context;
pub mod context_policy;
pub mod control;
pub mod events;
pub mod execution_input;
pub mod extensions;
pub mod tokens;
pub use execution_input::{ExecutionInput, ModelContextInfo};
pub mod input_context;
pub mod journal;
pub use input_context::{InputContextProvider, PreparedInput};
pub mod artifacts;
pub mod auxiliary;
pub mod media;
pub mod model;
pub mod redaction;
pub mod run;
pub mod session;
pub mod state;
pub mod summary;
pub mod tool_images;
pub mod tools;
pub mod turn_executor;
pub mod ui;

pub use agent::{
    Agent, AgentConfig, AgentTurnOutcome, AgentTurnResult, InterruptedAgentTurn, SessionSelection,
};
pub use cancellation::{TurnCancelled, TurnExecutionContext};
pub use completion::{
    AcceptAllCompletionVerifier, CompletionContext, CompletionVerdict, CompletionVerifier,
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

pub use journal::RunJournal;

pub use control::{
    ApprovalRequestId, CustomDelivery, CustomSubmission, CustomSubmissionFuture, InputSubmission,
    PendingRunApproval, RejectedCustomSubmission, RejectedInputSubmission, RejectedRunInput,
    RunControl, RunControlConfig, RunControlError, RunControlSnapshot, RunControlStatus, RunHandle,
    RunInput, RunInputKind,
};
