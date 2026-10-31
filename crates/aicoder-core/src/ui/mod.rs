//! Bounded ephemeral host UI. Answers are data, never tool authorization.
mod broker;
mod protocol;
pub use broker::{MAX_UI_SCOPE_DEPTH, UiClient, UiHost, UiLimits};
pub use protocol::*;
