# S6 native extension UI acceptance

Native extensions now have bounded ephemeral notifications, selection, text input, status
and real TUI/Web command adapters. Core supplies host/client data and cancellation contracts;
frontends own presentation and authenticated choices. SDK hosts opt in explicitly. A missing
UI returns Unsupported without reading stdin. See [contracts](extension-ui.md).

## Verification

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server --bins --examples
python3 scripts/check_extension_ui.py
python3 scripts/check_extension_ui_browser.py
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_images.py
python3 scripts/check_web_browser.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_process_protocol.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**563 default workspace tests, 606 all-feature tests and 226 isolated core tests** pass.
The feature adds fourteen core cases, four terminal cases, one runtime summary case and
three optional Web cases beyond the preceding 544/584/212 inventory:

- Headless/unoffered capabilities fail immediately; the same Agent/tool loop can use an
  explicit extension fallback. Typed choice/text answers reject invalid shapes, duplicate
  replies and foreign scopes without consuming a valid pending question. Unsafe control
  text and malformed prompts are not published.
- Pending questions, retained notices, status keys, serialized payloads and scope operations
  have independent bounds. Evictions report omitted retention. Cancellation, deadline,
  dropped futures and last-owner Drop expire questions; clients are weak and cannot revive
  an owner. Rebinding to a fresh context retains inherited cancellation; depth exhaustion
  fails closed. Callback timeout also invalidates retained client copies.
- Tools receive execution-scoped UI, while context hooks append only explicit callback
  output to the protected prefix. Only explicit tool/user data reaches normal history;
  question IDs/titles do not become facts. Manual summary retains the installed extension
  snapshot, obtains UI data, makes one tool-free structured summary attempt, commits its
  checkpoint and preserves raw canonical messages.
- Terminal selection uses explicit keys. Unicode/multiline paste stays a bounded independent
  field draft until Enter. Scope close clears statuses; dropping the terminal owner cancels
  pending data even if a separate UI observer remains. A new library entry reuses the same
  terminal/Agent loop rather than implementing a second executor.
- Authenticated loopback Web tests discover frozen commands, poll owned jobs, submit typed
  answers and reject extra authorization fields. A Notice makes zero model requests;
  Submit makes a normal AgentInput turn once. Reused IDs do not re-execute commands.
  Cancelled/foreign questions cannot reopen or submit a run; shutdown awaits the owner.
- A native before-tool hook asks for data, then the real write still requires its separate
  run approval. A UI reply cannot answer that approval or restore a session grant. Denial
  leaves the temporary file absent. Command cancellation/submission is serialized by the
  host-state lock; late terminal completion delivery cannot start a cancelled command's turn.

The new real PTY check drives the public terminal SDK host: choose, paste Unicode, submit
only a notice without creating a session, submit an explicit AgentInput through core and
cancel another dialog. It inspects actual JSONL user/turn records. The optional Chromium
check uses a fresh browser context and ephemeral fixture token: selection/input/status/
notices work, pasted HTML remains inert text, a submitted command's fast completed model
response is observed, and cancellation closes the question. The token is never printed.
Existing three terminal checks, browser checks and SDK/CLI JSON/RPC/Web parity remain green.
Published process/event/RPC/export fixtures retain 12/337/44/6 validated frames/entries.

One earlier all-feature batch, concurrent with release compilation, failed four existing
shell archive tests on missing artifact references. Their assertions were not weakened;
the isolated nine-case artifact suite and the final idle full suite passed. The precise
cause is unassigned; the shell backend's bounded storage deadlines remain unchanged.

## Default footprint

[Three final-source samples](baselines/s6-ui-report.json) share Rust/manifests/assets SHA-256
`d1c1d23e3b29db24484d7468d5064dd541d5be1d1f7967619bba9640142e2e73`.
The recorded revision is the parent because measurement precedes this feature commit.
Cargo.lock, dependency versions and the complete 271-package resolved inventory are unchanged.
No terminal/browser library enters core/runtime. Headless clients have no scope allocations;
UI scope state is created explicitly by frontend hosts.

| Metric | Final measurement | Change from local packages |
| --- | --- | --- |
| Release CLI | 17.044 MiB | -0.99% |
| Release TUI | 17.008 MiB | +3.17% |
| Release SWE-bench | 14.283 MiB | +0.25% |
| 125-case offline smoke wall time, median | 390.39 ms | +0.68% |
| Offline smoke peak RSS, median | 7,808 KiB | +1.67% |
| Idle offline runtime RSS, median | 4,192 KiB | +1.55% |

Wall samples are 390.39 / 382.93 / 395.76 ms; peak RSS is 7,808 / 7,808 / 7,680 KiB;
idle RSS is 4,204 / 4,160 / 4,192 KiB. Binary layout/codegen effects and individual component
costs are not isolated; the smaller CLI is an observation, not an optimization claim. Timing
and RSS noise do not establish an improvement or sustained regression. Existing targets/page
caches are reused, with no clean build or allocator instrumentation. Dialog rendering,
operator wait time and active host/process memory are outside this default scripted smoke.

## Scope

This completes the roadmap's native host UI interface and TUI/Web adapter path. Notification
acceptance means retained host state, not acknowledged rendering. UI snapshots are ephemeral,
not canonical event replay or restored authorization. Status keys clear on scope close; old
notices remain bounded for inspection. Scope tokens, UI data and registry/credential authority
remain separate. Ordinary tool/callback/run budgets still apply while waiting for user data;
interactive commands may explicitly declare up to 600 seconds, while hooks retain 30 seconds.

There is no implicit native-code/package startup, server-initiated process/MCP UI protocol,
CLI/RPC terminal reader or scripting runtime. Native host hooks can use the same UI around
registered process tools. Cooperative native code can still block a thread or perform direct
host IO; these APIs are not a sandbox. Normal terminal exit awaits owners; dropping a frontend
future signals cancellation and lets cleanup continue on its caller's live runtime.

Remaining S6 work includes plan/child-Agent/protected-path extension examples and optional
Code Mode. Remote package acquisition, further transport/UI capabilities, full Pi comparison
and S7 benchmark/release acceptance remain open. These scripted checks do not establish
real-model coding success, allocator costs, active-dialog throughput or combined process RSS.
