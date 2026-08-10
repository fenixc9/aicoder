//! Reusable core for building coding-agent frontends.
//!
//! The crate owns the model/tool loop and exposes its client, event, tool, and wire types so a
//! CLI, TUI, server, or embedded application can choose its own configuration and presentation.

pub mod agent;
pub mod client;
pub mod completion;
pub mod events;
mod redaction;
pub mod session;
pub mod state;
pub mod tools;
pub mod types;
pub mod workflow;

pub use agent::{Agent, AgentBuilder, AgentConfig, AgentRunResult, ChatCompletionProvider};
pub use client::{ChatClient, ClientConfig};
pub use completion::{
    AcceptAllCompletionVerifier, CompletionContext, CompletionVerdict, CompletionVerifier,
    WorkspaceChangeVerifier,
};
pub use events::{
    AgentEventHandler, AgentRawEvent, AgentRawEventEnvelope, CompletionVerificationOutcome, RunId,
    ToolExecutionOutcome,
};
pub use state::{
    AgentRunState, AgentRunStateMachine, AgentStateTransition, InvalidAgentStateTransition,
};
pub use workflow::{AgentTurnResult, AgentWorkflow, AgentWorkflowConfig, SessionSelection};
