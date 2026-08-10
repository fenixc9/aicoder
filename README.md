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
consumes the single AgentEventHandler interface. Every run follows an explicit
AgentRunState state machine and emits ordered StateChanged events, so CLI, TUI,
and web applications can present lifecycle state without reconstructing it from
lower-level model and tool events. Applications should depend on the core crate
rather than reproduce the loop.

Runtime prerequisites are Git, Bash, and ripgrep (`rg`). The SWE-bench grading
subcommand additionally requires the official Python package and Docker.

## Verification

~~~bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
~~~

See [the evaluation README](crates/aicoder-eval/README.md) for evaluation and
SWE-bench usage.
