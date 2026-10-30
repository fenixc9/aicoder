//! Optional local Web host example; enable `server` to build its adapter and binary.
//! Core and runtime remain independent of HTTP and browser types.
#![cfg(feature = "server")]
mod host;
mod http;
pub use host::{WebHost, WebOptions};
