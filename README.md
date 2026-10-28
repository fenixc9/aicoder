# aicoder

aicoder is a small Rust coding-agent workspace. The reusable execution and
conversation APIs live in aicoder-core; applications and evaluation tooling
are separate crates.

## Workspace

- aicoder-core: the low-level TurnExecutor plus the session-aware Agent API,
  model interfaces, events, tools, and session repositories.
- aicoder-providers: model-service clients, HTTP transport, retries, and
  OpenAI-compatible response streaming.
- aicoder-cli: current command-line application.
- aicoder-tui: interactive Ratatui frontend with sessions, streaming output,
  tool approvals, execution status, and cooperative cancellation.
- aicoder-eval: isolated evaluation runner, deterministic evaluators,
  SWE-bench adapter, batch runner, and official harness integration.

Each package lives under `crates/`; the root manifest defines the Cargo
workspace and shared dependency versions. See [contributing](CONTRIBUTING.md)
and [project instructions](AGENTS.md) for development conventions.

The [development roadmap](docs/roadmap.md) defines the plan for a lightweight,
extensible core and Pi feature parity across SDK, CLI, TUI, and Web integrations.
See [API migrations](docs/migrations.md) when updating an embedded application.
The [provider-neutral model API](docs/model-api.md) is shared by the agent
loop, tools, sessions and frontends. Structured [run outcomes and budgets](docs/run-outcomes.md) retain progress on
limits, failures and cancellation. [Provider profiles](docs/provider-configuration.md) configure proxy, credentials,
timeouts and compatible-service behavior. [Tree sessions and recovery](docs/sessions.md)
cover branches, durable tool checkpoints, interrupted turns and backed-up v1/v2 migration.
[RunHandle controls](docs/run-control.md) add bounded steer/follow-up queues and scoped
asynchronous approvals over the same execution loop. [Versioned events](docs/events.md)
provide bounded subscriptions, explicit overflow/disconnect reports, and structured
bench traces. S0–S2 are complete; shared runtime/project context is next in S3.

An application constructs a TurnExecutor and wraps it in Agent when it needs
request construction or session persistence. Both layers consume the single
AgentEventHandler interface. Every run follows an explicit
AgentRunState state machine and emits ordered StateChanged events, so CLI, TUI,
and web applications can present lifecycle state without reconstructing it from
lower-level model and tool events. Applications should depend on the core crate
rather than reproduce the loop.

Long-running sessions can opt into context-window budgeting through a pluggable
ContextCompactor. See [context compaction](docs/context-compaction.md) for the
first deterministic policy and the semantic-summary extension boundary.

Interactive frontends can pass a cloneable `TurnExecutionContext` to `Agent` or
`TurnExecutor` and call `cancel(reason)` to cooperatively stop the active model
request, stream, approval, compaction, verification, or tool execution. A
cancelled run transitions to `AgentRunState::Aborted`, emits `AgentAborted`, and
returns a downcastable `TurnCancelled` error.

Runtime prerequisites are Git, Bash, and ripgrep (`rg`). The SWE-bench grading
subcommand additionally requires the official Python package and Docker.

## TUI

Set an OpenAI-compatible API key and optional endpoint, then start the TUI from
the workspace you want the agent to edit:

~~~bash
export OPENAI_API_KEY=...
export OPENAI_API_BASE=https://api.openai.com/v1
cargo run -p aicoder-tui -- --workspace . --model gpt-4o
~~~

Pass `--session <ID>` to open an existing session for the selected workspace.

Use `Tab` to switch between the session list and input, `Enter` to send or open
a session, and `j`/`k` to navigate sessions. `n` creates a new conversation and
`d` deletes the selected session after confirmation. During a run, Enter submits
steering and Alt+Enter queues a follow-up. `Esc`
cancels the active model request, approval, or tool execution. Approval dialogs
accept `y` or `n`; `Ctrl-C` cancels an active turn or exits while idle. TUI logs
are written to `$AICODER_HOME/tui.log` or `~/.aicoder/tui.log`.

## Verification

~~~bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo run --locked -p aicoder-core --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
~~~

See [the evaluation README](crates/aicoder-eval/README.md) for evaluation and
SWE-bench usage.
See [the development baseline](docs/development-baseline.md) for toolchain and
platform support, API/session format snapshots, and performance measurements.
