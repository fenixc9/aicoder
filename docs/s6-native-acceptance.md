# S6 native extension registration and hooks

The first two S6 items now have a runnable native Rust path: register provider routes,
tools, context policies, context/tool hooks, observers and commands; install one immutable
snapshot in the shared Agent loop. [The API contract](native-extensions.md) defines ordering,
timeouts, errors, mutable fields and authority. [Migrations](migrations.md) describe the
additive builder/input-context API. No session/event format version changes.

## Evidence

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-core --example native_extensions
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server --bins --examples
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_images.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**524 default workspace tests**, **534 all-feature tests**, and **211 isolated core tests**
pass. Fifteen additions extend the prior 509/519/198 inventory:

- Failed registration publishes no part of a conflicting extension. Names/counts/command
  reservations are bounded; contributed tool names/capabilities remain pinned even if the
  underlying object's metadata later changes.
- Context hooks run in configured order, see prior contributions and retain canonical
  input. Resume does not duplicate prior hook prefixes. An explicit projected recovery
  system marker remains outside the declared host prefix and does not become history.
- Guards precede read-only preparation and approval-cache lookup. The allowed call's
  approved original arguments/preview reach the same owned plan. A later denial still
  blocks a cached session grant; a replacement snapshot requires fresh approval authority.
- Timeout, panic, returned error and oversized context fail before user/turn writes.
  Tool-hook failures yield paired failures without preparation, approval or mutation.
  Raw returned error strings do not leak into loop errors or model-facing tool results.
- Parent cancellation drops context/tool/command futures and cancels their private tokens;
  no late result is delivered. Tool cancellation closes the session journal without execution.
- Commands return explicit host-delivered input, reject oversized arguments/notices/input
  envelopes and time out cooperatively. Submitted text is bounded without an extra copy.
- Host/typed observer callbacks run once in order. Panics and blocked-drain timeouts produce
  explicit delivery failures while the authoritative agent result remains available.
- A named host-selected extension policy cannot keep requests beyond known model windows;
  provider registration does not select an unrequested route.
- Runtime composes pinned AGENTS.md resources with native context, persists/replays JSONL
  history without storing hook text as user facts, and uses the declared prefix boundary.

The pure-core [native example](../crates/aicoder-core/examples/native_extensions.rs)
installs all seven contribution kinds. Its embedded plan command returns user input which
the host explicitly submits through Agent. Planning produces extension_denied; leaving
planning produces approval_denied under the default host policy. **Zero mutations**, two
scripted turns and **60 observed events** confirm that neither command nor guard
creates approval authority. The example needs no runtime, credentials or HTTP client.

The three existing real PTY checks pass, preserving paste/approval separation, owner cleanup,
image snapshots and queued input. SDK/CLI JSON/RPC/Web still produce the same normalized
scripted result. Existing schema fixtures validate 337 event frames, 44 RPC frames and six
export entries. All model services in tests are scripted or loopback with fake credentials.

## Scope

This delivers native SDK registration/interception and the runtime assembly helper. Stock
frontends do not yet load extension packages or expose these native command registrations.
Dynamic process tools, MCP, package installation/reload, UI capabilities and the remaining
plan/sub-agent/protected-path examples remain unchecked S6 work. The plan example demonstrates
this API; it does not complete the entire later example/UI/package checklist or Pi parity.

Native implementations are trusted host code. Cooperative callback/serialization/queue
limits do not enforce a process memory sandbox or preempt blocking Rust/syscalls. Observers
share the existing bounded callback consumer; a failed observer can stop that consumer's
remaining delivery, and timeout does not kill its thread. Returned errors omit raw details;
the process panic hook remains host-owned. Catalog/history records cannot recreate native
objects, choose credentials or restore approval grants. No dynamic-library ABI or scripting
runtime is introduced.

## Footprint

[Three final-source samples](baselines/s6-native-report.json) use source SHA-256
`0c1cec311ea747d64e2dfcb8b429f0baaebefcf6dc6e780cdaaa539905913d8f` and the prior default
text/tool workload with **no extension installed**. Revisions identify the parent because
measurement precedes the feature commit. Cargo.lock and dependency manifests are unchanged.

| Metric | Final measurement | Change from S5 images |
| --- | --- | --- |
| Default resolved packages | 271 | unchanged |
| Release CLI | 16.687 MiB | +0.99% |
| Release TUI | 16.149 MiB | +0.98% |
| Release SWE-bench | 13.893 MiB | +1.09% |
| Offline smoke wall time, median | 392.16 ms | +0.88% |
| Offline smoke peak RSS, median | 7,680 KiB | +1.69% |
| Idle offline runtime RSS, median | 4,084 KiB | -1.92% |

Wall samples are 387.63 / 741.76 / 392.16 ms. Peak RSS is 7,680 / 7,424 / 7,808 KiB;
idle RSS is 4,084 / 4,160 / 4,044 KiB. The outlier includes one approximately 370 ms
SessionResume case; its cause is unassigned and all observations are retained. These local
samples do not establish a speed improvement or a sustained timing regression. They reuse
targets/page caches and do not measure clean compilation, installed-hook throughput,
rendered TUI idle, real model latency or coding success. S7 still owns those benchmark claims.
