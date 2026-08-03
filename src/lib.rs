//! Reusable core for building coding-agent frontends.
//!
//! The crate owns the model/tool loop and exposes its client, event, tool, and wire types so a
//! CLI, TUI, server, or embedded application can choose its own configuration and presentation.

pub mod agent;
pub mod client;
pub mod events;
mod redaction;
pub mod tools;
pub mod types;

pub use agent::{Agent, AgentBuilder, AgentConfig, AgentRunResult, ChatCompletionProvider};
pub use client::{ChatClient, ClientConfig};
pub use events::{
    AgentRawEvent, AgentRawEventEnvelope, AgentTypeEventHandler, RunId, ToolExecutionOutcome,
};
