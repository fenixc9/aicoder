//! Model service adapters for aicoder. HTTP and service configuration live here, not in core.
//!
//! The OpenAI-compatible adapter implements the provider-neutral model contract.
//! Protocol-specific request, response and stream types live in `wire`.

mod chat;
mod event_sink;
mod model;
mod stream;

pub(crate) use event_sink::ProviderEventSink;

pub use chat::{ChatClient, ClientConfig};

pub mod wire;
