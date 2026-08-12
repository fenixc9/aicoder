//! Reusable core for building coding-agent frontends.
//!
//! The crate owns the model/tool loop and exposes its client, event, tool, and wire types so a
//! CLI, TUI, server, or embedded application can choose its own configuration and presentation.

pub mod agent;
pub mod cancellation;
pub mod client;
pub mod completion;
pub mod context;
pub mod events;
mod redaction;
pub mod session;
pub mod state;
pub mod tools;
pub mod turn_executor;
pub mod types;

pub use agent::{Agent, AgentConfig, AgentTurnResult, SessionSelection};
pub use cancellation::{TurnCancelled, TurnExecutionContext};
pub use client::{ChatClient, ClientConfig};
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
pub use turn_executor::{
    ChatCompletionProvider, TurnExecutionConfig, TurnExecutionResult, TurnExecutor,
    TurnExecutorBuilder,
};
