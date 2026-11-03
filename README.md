# aicoder

aicoder is a small Rust coding-agent workspace. The reusable execution and
conversation APIs live in aicoder-core; applications and evaluation tooling
are separate crates.

## Workspace

- aicoder-core: the low-level TurnExecutor plus the session-aware Agent API,
  model interfaces, events, tool/store contracts, and memory sessions.
- aicoder-providers: model-service clients, HTTP transport, retries, and
  OpenAI-compatible response streaming.
- aicoder-runtime: shared settings, project resources, credential references, coding tools,
  JSONL sessions, Git verification, and default application assembly.
- aicoder-cli: current command-line application.
- aicoder-tui: interactive Ratatui frontend with sessions, streaming output,
  tool approvals, execution status, and cooperative cancellation.
- aicoder-eval: isolated evaluation runner, deterministic evaluators,
  SWE-bench adapter, batch runner, and official harness integration.
- aicoder-web: optional loopback HTTP/SSE/browser example (`server` feature).
- aicoder-codemode: optional bounded QuickJS programs (`engine` feature); disabled in
  default runtime/frontend builds.

Each package lives under `crates/`; the root manifest defines the Cargo
workspace and shared dependency versions. See [contributing](CONTRIBUTING.md)
and [project instructions](AGENTS.md) for development conventions.

The [development roadmap](docs/roadmap.md) defines the plan for a lightweight,
extensible core and Pi feature parity across SDK, CLI, TUI, and Web integrations.
See [API migrations](docs/migrations.md) when updating an embedded application.
[Native extensions](docs/native-extensions.md) register tools, provider routes, policies,
context/guard hooks, observers and commands explicitly in Rust hosts; the same core loop
retains approval, cancellation, budgets and canonical sessions.
[Explicit host launch files](docs/host-extensions.md) enable process/MCP tools in CLI, TUI and Web.
[Process tool extensions](docs/process-extensions.md) add an optional versioned stdio adapter
with owned-plan approval, bounded transport and process-group cleanup; no scripting runtime
is part of the default build. [MCP stdio](docs/mcp.md) supports modern per-request metadata
and legacy initialization, with bounded discovery, host approvals and scoped media artifacts.
[Resource packages](docs/packages.md) add local manifest installation, exact content locks,
frozen reload generations and public session state; executable activation remains explicit
in SDK hosts. [Extension UI](docs/extension-ui.md) provides scoped native notifications, questions, input,
status and commands with real TUI/Web adapters and explicit headless behavior.
[Coding extension examples](docs/coding-extension-examples.md) demonstrate planning, guarded
prepared paths and explicitly managed read-only worker Agents over the same loop.
[Code Mode](docs/code-mode.md) optionally composes real registered tools in JavaScript,
with discovery/filtering and branch-local state while preserving per-tool approvals and
parent budgets. Enable the host's `code-mode` build feature and `--code-mode` setting.
Canonical sessions support [bounded JSON/HTML exports](docs/session-exports.md), with
explicit publication adapters and no automatic third-party sharing.
The [provider-neutral model API](docs/model-api.md) is shared by the agent
loop, tools, sessions and frontends. Structured [run outcomes and budgets](docs/run-outcomes.md) retain progress on
limits, failures and cancellation. [Provider profiles](docs/provider-configuration.md) configure proxy,
credentials, timeouts and compatible-service behavior. [Reasoning controls](docs/reasoning-controls.md)
expose declared effort levels through SDK, CLI and TUI. [Image input](docs/image-input.md)
uses the same Agent API, with bounded local captures via CLI `--image` and TUI `/image`.
[Tool/MCP images](docs/tool-images.md) use scoped artifacts and bounded model-only previews,
while canonical history retains references. Native `read_file` can acquire all five supported
image formats, including BMP, under the same workspace and storage bounds. [Tree sessions and recovery](docs/sessions.md)
cover branches, durable tool checkpoints, interrupted turns and backed-up v1/v2 migration.
[RunHandle controls](docs/run-control.md) add bounded steer/follow-up queues and scoped
asynchronous approvals over the same execution loop. [Versioned events](docs/events.md)
provide bounded subscriptions, explicit overflow/disconnect reports, and structured
bench traces. S0–S3 are complete on the verified Linux platform. Runtime provides
[shared settings](docs/runtime-configuration.md), [project resources](docs/project-resources.md),
[concrete tools/storage](docs/runtime-backends.md), [bounded read/search](docs/file-tools.md),
[checked edits and diffs](docs/file-mutations.md), [shell supervision](docs/shell-tools.md)
and [session tool grants](docs/approvals.md). S4 long-context management is complete;
S5 frontends have passed [Linux acceptance](docs/s5-images-acceptance.md); S6 extensions are implemented; S7 evaluation and the final Pi comparison remain. [Branch model-context projections](docs/model-context.md) keep recovery
views separate from recorded history. See the settings guide for the explicit `--env-file` migration and
[S3 acceptance](docs/s3-approvals-acceptance.md) for verification and cumulative costs.


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

## CLI

With a provider configured through the [shared settings](docs/runtime-configuration.md):

~~~bash
cargo run --locked -p aicoder-cli -- --print 'inspect the project'
printf 'explain this project\n' | cargo run --locked -p aicoder-cli -- --json
cargo run --locked -p aicoder-cli -- --repl
~~~

`-p` remains `--prompt`; use `--print` for final text or `--json` for event-v1 JSONL.
REPL reuses core sessions; `--no-session` retains history in memory. Piped input is
prompt data and cannot approve tools. Ctrl-C cancels and awaits the active turn.
See [CLI modes](docs/cli-execution.md) for input bounds, approvals and exit behavior.

`--rpc` serves [versioned JSONL requests](docs/rpc.md) with run controls, approvals,
session selection and events. An optional [Python client example](scripts/rpc_client.py)
shows the request/notification lifecycle; Python is not required by the agent binary.

## TUI

[TUI interaction](docs/tui-interaction.md) includes multiline/grapheme editing, bounded input
history, immutable `/file PATH` pages, frontend-only themes, and live context counts with
optional declared base-rate estimates. Use Shift+Enter or Ctrl+O for a newline, Ctrl+P/N for
history, `/detach` to clear references, `/theme dark|light|mono`, and `/usage` for provenance.
Bracketed paste is draft data and cannot answer approval dialogs. `--theme-file PATH` loads
a bounded explicit palette without putting UI types/settings into core or model prompts.

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
accept `y` once, `s` for the displayed session/tool scope, or `n`;
`/permissions` and `/revoke` inspect or clear grants while idle. `Ctrl-C` cancels an active turn or exits while idle. TUI logs
are written to `$AICODER_HOME/tui.log` or `~/.aicoder/tui.log`.

## Web example

The [optional Web host](docs/web.md) provides HTTP submission, SSE, cancellation and
prepared approval choices for one host-selected workspace. Set a private
`AICODER_WEB_TOKEN` and run `cargo run --locked -p aicoder-web --features server -- --workspace .`.
Normal CLI/TUI/core/runtime builds do not enable the Web server dependencies.

## Verification

~~~bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
~~~

[Coding benchmark archives](docs/coding-benchmarks.md) run a pinned Rust/JSON task suite directly
through core/runtime, with hidden post-run grading, verifiable events/diffs and input-checked
comparisons. `aicoder-bench run` defaults to offline scripted contracts; service runs require
explicit `--live`. Contract pass rates are separate from real-model coding success.

[SWE-bench contracts](docs/swebench-contract.md) pin generation/resume inputs and official
5.0.2 grading, preserve failed samples in the selected denominator, and isolate cache/cleanup
ownership. The optional Docker fixture verifies interoperability without real model calls.

See [the evaluation README](crates/aicoder-eval/README.md) for evaluation and
SWE-bench usage.
See [the development baseline](docs/development-baseline.md) for toolchain and
platform support, API/session format snapshots, and performance measurements.
