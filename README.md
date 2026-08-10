# aicoder

aicoder is a small Rust coding-agent workspace. The reusable execution and
conversation APIs live in aicoder-core; applications and evaluation tooling
are separate crates.

## Workspace

- aicoder-core: model client, agent loop, events, tools, sessions, and the
  application-level AgentWorkflow.
- aicoder-cli: current command-line application.
- aicoder-eval: isolated evaluation runner, deterministic evaluators,
  SWE-bench adapter, batch runner, and official harness integration.

An application constructs an Agent, optionally wraps it in AgentWorkflow, and
consumes the single AgentEventHandler interface. CLI, TUI, and web applications
should depend on the core crate rather than reproduce the loop.

## Verification

~~~bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
~~~

See [the evaluation README](crates/aicoder-eval/README.md) for evaluation and
SWE-bench usage.
