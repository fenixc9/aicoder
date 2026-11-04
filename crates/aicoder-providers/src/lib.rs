//! Model service adapters for aicoder. HTTP and service configuration live here, not in core.
//!
//! The OpenAI-compatible adapter implements the provider-neutral model contract.
//! Protocol-specific request, response and stream types live in `wire`.

#[cfg(feature = "auxiliary-models")]
pub mod auxiliary;
mod chat;
mod config;
mod credentials;
mod error;
mod event_sink;
#[cfg(feature = "auxiliary-models")]
pub mod image_generation;
mod model;
mod stream;
pub mod transport;
mod transport_client;

pub(crate) use event_sink::ProviderEventSink;

pub use chat::ChatClient;
pub use config::{ClientConfig, CompatibilityProfile, ProxyMode};
pub use credentials::{BearerToken, CredentialProvider, EnvironmentCredential, StaticCredential};

pub mod wire;

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierImageInput {
    #[default]
    Assets,
    StateImageUrls,
    Images,
}
