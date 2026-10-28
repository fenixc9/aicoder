# API migrations

## S1: model clients move to `aicoder-providers`

`ChatClient` and `ClientConfig` now live in the providers crate. Applications
using the built-in HTTP adapter should update their dependency and imports:

```toml
[dependencies]
aicoder-core = { path = "../aicoder/crates/aicoder-core" }
aicoder-providers = { path = "../aicoder/crates/aicoder-providers" }
```

```rust,ignore
use aicoder_core::{Agent, AgentConfig, TurnExecutor};
use aicoder_providers::{ChatClient, ClientConfig};
```

The core crate no longer exports `client`, `ChatClient`, or `ClientConfig`.
It does not re-export providers, which would introduce a dependency cycle.
The `ChatCompletionProvider` trait, builder, and model-client methods retain
their behavior in this step. CLI, TUI, and the SWE-bench binary use the new
adapter crate. Session v1 files are unchanged.

Custom providers implementing `ChatCompletionProvider` need no providers
dependency. They can still use the core event emitter. Its constructor and
shutdown method are public for provider integration and testing; the owner
must finish producers and await shutdown before examining a final event trace.

This is only the transport separation step of S1. OpenAI-style wire types
remain in core pending the protocol-neutral model contract migration. Core's
URL parser is used for credential redaction and does not perform HTTP.

The [transport-separation baseline](baselines/s1-transport-report.json) reruns
the same 125 offline cases after extraction. All cases pass. In this single
sample, application binaries grow by 0.7–1.2%, clean release compilation takes
44.0 seconds, and the offline runtime peaks at 4.50 MiB RSS. Timing and RSS are
diagnostic samples, not statistical proof of improvement. HTTP/SSE behavior
is covered by the migrated provider tests as well as the workspace checks.

## S1: provider-neutral model contract

`aicoder_core::model` now provides `Message`, `ContentBlock`, `ModelRequest`,
`ModelResponse`, `ModelUsage`, `ModelSpec`, `ModelRegistry`, `ProviderError`,
`ModelEventSink` and `ModelProvider`. `ChatClient` implements `ModelProvider`
through validated request/response conversion. See [the model API guide](model-api.md)
and run `cargo run --locked -p aicoder-core --example model_provider` for an
in-memory implementation without a model-service dependency.

Existing `ChatCompletionProvider`, `TurnExecutor` and session APIs are retained
in this migration step. Applications can call `ModelProvider::generate` now;
the next S1 change migrates the execution/session/tool path to the new types
and retires the legacy public message types. This is not a second agent loop.
