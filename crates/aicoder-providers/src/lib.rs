//! Model service adapters for aicoder. HTTP and service configuration live here, not in core.
//!
//! The OpenAI-compatible adapter implements the provider-neutral model contract.
//! Protocol-specific request, response and stream types live in `wire`.

mod chat;
mod config;
mod credentials;
mod error;
mod event_sink;
mod model;
mod stream;

pub(crate) use event_sink::ProviderEventSink;

pub use chat::ChatClient;
pub use config::{ClientConfig, CompatibilityProfile, ProxyMode};
pub use credentials::{BearerToken, CredentialProvider, EnvironmentCredential, StaticCredential};

pub mod wire;
