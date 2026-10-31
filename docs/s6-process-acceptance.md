# S6 versioned process tools

S6's process-extension item now has a runnable optional stdio adapter: an explicit Rust
host launches a server, negotiates aicoder.tools.v1, freezes its bounded tool catalog into
the existing native registry and runs those tools through core approval/budgets/journaling.
The [contract](process-extensions.md), [JSON Schema](protocol/process-tools-v1.schema.json)
and [synthetic wire fixture](protocol/process-tools-v1.jsonl) define the supported profile.
No dynamic-library ABI, default process startup or scripting runtime is introduced.

## Verification

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo test --locked -p aicoder-runtime --features process-extension-fixtures --test process_extensions
cargo test --locked -p aicoder-runtime --test shell_tools
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo build --locked -p aicoder-runtime --features process-extension-fixtures --bin aicoder-process-fixture
cargo run --locked -p aicoder-runtime --features process-extensions --example offline_process_extensions
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-runtime --features process-extensions --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server --bins --examples
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_images.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_process_protocol.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**525 default workspace tests**, **548 all-feature tests** and **212 isolated core tests**
pass. One public wire-contract test and 13 optional real-process tests extend the prior
524/534/211 inventory. The fixture is a separately compiled Rust executable; these tests
need no Node, Python, credentials or remote model service.

- The published 12 synthetic request/reply frames match public Rust DTO serialization,
  and validate against the versioned JSON Schema. The fixture contains example tokens,
  not real session history or transferable backend authority.
- Prepared remote writes use the same runtime mutation backend: the reviewed captured
  preview/arguments reach the original owned plan; denial disposes it, and external file
  changes before approval cause conflict without overwriting that change. JSONL records
  contain canonical calls/results and no actual remote plan tokens.
- Plan admission is bounded; dropping a plan receives disposal acknowledgement. A live
  old plan cannot commit through a replacement connection. Aliases/capabilities/identity
  checks are host-selected; even a read tool defaults to Mutating. Workspace mismatch
  closes the connection instead of dispatching into another root.
- Unknown versions, duplicate catalogs, oversized schemas, malformed frames and stderr
  floods fail initialization and reap the owned leader. Wrong IDs, frame overflow and
  partial replies poison active calls without retries; a crash preserves exit code 7 and
  uncertainty. Complete 128 KiB replies followed immediately by exit remain completed;
  duplicate replies close the connection without repeating or discarding that completion.
- Core cancellation covers both an unprepared call and an approved prepared commit:
  the owner journal closes, the group is signalled, its leader is reaped and the target
  stays unchanged. Queued cancellation leaves an unrelated active call running; queue-full
  and oversized inputs are explicitly unsent. Same-group descendants are terminated.
- Deadlines, initialization cancellation, last-client drop and Tokio runtime teardown
  all trigger runtime-independent cleanup. Tests await both kernel disappearance and
  final worker metadata where appropriate, rather than treating a scheduling gap as failure.
- Launch/debug/tool errors omit argv/environment values, raw stderr and peer error text.
  Launch failure reports no child; numeric remote error codes remain available. Incomplete
  cleanup is distinct from confirmed reaping, and worker completion follows pending reply
  resolution. No automatic restart can resurrect a tool connection or session authority.

The host [offline example](../crates/aicoder-runtime/examples/offline_process_extensions.rs)
executes one approved external write in two model rounds, discards a second captured plan
and waits for its acknowledgement, then confirms reaping. It reports five sent/completed
RPC requests and zero network requests. CI builds/runs the explicit fixture/example; the
all-feature suite runs its process fault cases. All 14 existing Bash tests still pass after
extracting shared leader/group ownership. Existing PTY/SDK/CLI JSON/RPC/Web parity and the
337 event / 44 frontend RPC / six export fixture checks remain valid.

## Scope and limits

Only the stdio aicoder.tools.v1 profile is delivered here. It is not MCP compatibility or
an automatic CLI/TUI/Web package loader. MCP, pinned resource packages/reload, UI adapters,
remaining extension examples and Code Mode stay unchecked in S6; S7 benchmark/release
work remains open. External authors may implement the published protocol in other languages;
the fault-injecting fixture is a test/demo executable, not a production server framework.

Programs are trusted host-selected code with user permissions; preparation/read-only
claims cannot sandbox a dishonest server. Environment inheritance is off, but programs
may access the filesystem beyond cwd. Detached groups and stuck OS syscalls are outside
process-group containment. Active cancellation poisons a shared connection for queued
callers; hosts use separate connections for independent lifecycles. Byte counters describe
observed transport reads, not complete raw logs. Peer truncation is distinct from complete
RPC framing; host artifacts remain explicit core storage. No schema/version change to
canonical sessions or event v1 is needed, and no opaque plan token becomes domain history.

## Default footprint

[Three final-source samples](baselines/s6-process-report.json) identify Rust/manifests/assets
by SHA-256 `95546f4ef10474f3e2af16b98f48aaecffcf81ae9c5b6232b90a5a10a45528d4`.
Revisions identify the parent because measurements precede this commit. No dependency
is added and Cargo.lock is unchanged. The optional process adapter/fixture is disabled in
the measured default build and has no default thread/server startup.

| Metric | Final measurement | Change from S6 native |
| --- | --- | --- |
| Default resolved packages | 271 | unchanged |
| Release CLI | 16.681 MiB | -0.04% |
| Release TUI | 16.143 MiB | -0.04% |
| Release SWE-bench | 13.888 MiB | -0.04% |
| Offline smoke wall time, median | 387.07 ms | -1.30% |
| Offline smoke peak RSS, median | 7,680 KiB | unchanged |
| Idle offline runtime RSS, median | 4,096 KiB | +0.29% |

Wall samples are 388.88 / 385.68 / 387.07 ms. Peak RSS is 7,680 / 7,680 / 7,552 KiB;
idle RSS is 4,096 / 4,124 / 4,080 KiB. Targets/page caches are reused; these small local
differences do not establish a speed/size improvement. Clean compilation, active process
throughput/idle RSS, combined host/server memory, actual provider latency and coding success
are not measured by the default smoke. Those remain S7 benchmark work.
