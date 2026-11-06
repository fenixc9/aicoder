# S7 busy custom-delivery acceptance

RunHandle supports custom steering/follow-up, safe context without forced continuation and
next-turn session delivery. It shares existing reservation, journal and model/tool boundaries.
Candidate checkpoints precede deferred records; canonical entries remain Custom. Bounded opaque
inbox claims commit only after the initial turn transaction acknowledges, retain new arrivals,
release after failure/drop and never restore queue authority from history.

Sixteen core tests cover all deliveries, canonical order and continuation, next-turn isolation /
consumption, preparation failure, settlement arrivals, cancellation/pending recovery, session
bounds/discard/restore separation, unsupported journals, paired tool boundaries, queue/body/late
limits, claims/new arrivals/busy discard, failed and slow checkpoints with paid usage retained,
handled initial input, known windows, explicit generation transfer and origin receipts. The
verification-retry regression failed before the fix: context arriving during rejected verification
was missing from the immediate retry request. It now commits before retry; safe next-round drains
also cover arrivals between round boundaries. Two runtime tests exercise all four deliveries in
normal and Code Mode with actual owned file tools, allow/deny, parent usage, JSONL, archives,
inert export and event validation. TUI respects hidden custom rows; schema checks reject invalid
user/custom role pairing. Existing input/custom PTY and authenticated host regressions pass.

Ten executable variants independently exercise four normal/four Code Mode deliveries and two
approval-denial cases. Follow-up uses three current-run requests / 21 fixture tokens; other modes
use two / 14. NextTurn waits for a second explicit trigger (three total requests), with two real
User messages; other modes retain one User. Every variant records exactly one custom message,
keeps inert details out of prompts and performs no external model/network request.

Pinned Rust 1.92.0 / Linux passed **84 final gates, 809 default workspace tests,
939 all-feature tests and 351 isolated core tests**. Standalone codec/JS-free runtime
selects 1 custom-delivery test; Code Mode adds the second. Prior native/provider transport,
images, raw input, startup/boundaries, custom messages, frontends, schemas, benchmark and SWE
bridge regressions passed. Commands:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked -p aicoder-runtime --no-default-features --test image_files --lib
cargo test --locked -p aicoder-runtime --no-default-features --features code-mode --test code_mode_images
cargo test --locked -p aicoder-runtime --no-default-features --features auxiliary-models --test auxiliary_models
cargo test --locked -p aicoder-runtime --no-default-features --features auxiliary-models --test image_generation
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-runtime -p aicoder-eval --features server,host-extensions,mcp-fixtures,images --bins --examples
cargo build --locked -p aicoder-web --features server,host-extensions,images
python3 scripts/check_file_images.py
python3 scripts/check_host_extensions.py --images
python3 scripts/check_host_extensions.py --images --mime-alias
python3 scripts/check_images.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_event_schema.py
python3 scripts/check_export_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server,images,code-mode --bins
python3 scripts/check_file_images.py --code-mode
python3 scripts/check_file_images.py --only
python3 scripts/check_file_images.py --classifier
python3 scripts/check_file_images.py --generate-images
cargo test --locked -p aicoder-runtime --no-default-features --test tool_interception
cargo run --locked -p aicoder-runtime --example offline_tool_interception
cargo run --locked -p aicoder-runtime --example offline_tool_interception -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_tool_interception -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_tool_interception -- --code-mode --deny
cargo test --locked -p aicoder-runtime --no-default-features --test request_views
cargo run --locked -p aicoder-runtime --example offline_request_views
cargo run --locked -p aicoder-runtime --example offline_request_views -- --stream
cargo test --locked -p aicoder-providers --test transport
cargo test --locked -p aicoder-runtime --no-default-features --test transport_hooks
cargo run --locked -p aicoder-runtime --example offline_transport_hooks
cargo run --locked -p aicoder-runtime --example offline_transport_hooks -- --stream
cargo test --locked -p aicoder-providers --all-features --test transport
cargo test --locked -p aicoder-runtime --no-default-features --features auxiliary-models --test auxiliary_transport
cargo run --locked -p aicoder-runtime --features auxiliary-models --example offline_auxiliary_transport
cargo run --locked -p aicoder-runtime --features auxiliary-models --example offline_auxiliary_transport -- --observer-error
cargo run --locked -p aicoder-runtime --features auxiliary-models --example offline_auxiliary_transport -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_auxiliary_transport -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_auxiliary_transport -- --code-mode --deny
cargo test --locked -p aicoder-runtime --no-default-features --test boundary_hooks
cargo run --locked -p aicoder-runtime --example offline_boundary_hooks
cargo run --locked -p aicoder-runtime --example offline_boundary_hooks -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_boundary_hooks -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_boundary_hooks -- --code-mode --deny
cargo test --locked -p aicoder-runtime --no-default-features --test startup_hooks
cargo run --locked -p aicoder-runtime --example offline_startup_hooks
cargo run --locked -p aicoder-runtime --example offline_startup_hooks -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_startup_hooks -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_startup_hooks -- --code-mode --deny
cargo test --locked -p aicoder-runtime --no-default-features --test input_hooks
cargo run --locked -p aicoder-runtime --example offline_input_hooks
cargo run --locked -p aicoder-runtime --example offline_input_hooks -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_input_hooks -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_input_hooks -- --code-mode --deny
cargo run --locked -p aicoder-runtime --example offline_input_hooks -- --handled
python3 scripts/check_input_hooks.py
cargo test --locked -p aicoder-runtime --no-default-features --test custom_messages
cargo run --locked -p aicoder-runtime --example offline_custom_messages
cargo run --locked -p aicoder-runtime --example offline_custom_messages -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_custom_messages -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_custom_messages -- --code-mode --deny
cargo run --locked -p aicoder-runtime --example offline_custom_messages -- --record-only
python3 scripts/check_custom_messages.py
cargo test --locked -p aicoder-runtime --no-default-features --test custom_delivery
cargo run --locked -p aicoder-runtime --example offline_custom_delivery -- --delivery steer
cargo run --locked -p aicoder-runtime --example offline_custom_delivery -- --delivery follow_up
cargo run --locked -p aicoder-runtime --example offline_custom_delivery -- --delivery turn_end
cargo run --locked -p aicoder-runtime --example offline_custom_delivery -- --delivery next_turn
cargo run --locked -p aicoder-runtime --example offline_custom_delivery -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_custom_delivery -- --code-mode --delivery steer
cargo run --locked -p aicoder-runtime --features code-mode --example offline_custom_delivery -- --code-mode --delivery follow_up
cargo run --locked -p aicoder-runtime --features code-mode --example offline_custom_delivery -- --code-mode --delivery turn_end
cargo run --locked -p aicoder-runtime --features code-mode --example offline_custom_delivery -- --code-mode --delivery next_turn
cargo run --locked -p aicoder-runtime --features code-mode --example offline_custom_delivery -- --code-mode --deny
```

Local checks are not hosted CI results. The image-generation PTY check initially timed out
waiting for exit after canonical completion; an unchanged-source standalone repeat passed.
The report retains the initial exit and retry observation; no causal diagnosis is claimed. See [contract](custom-delivery.md) and
[migration](migrations.md) for optional RunInput.custom, Context-only kind, journal support and
explicit inbox ownership. No dependencies, manifests, lockfile or default features changed.
Session versions remain unchanged; v1 event schema adds typed custom controls. Message transforms,
session lifecycle/navigation veto, boundary context-edit/compaction, final isolated Spark/Pi
comparison and release acceptance remain open in the [audit](pi-parity-audit.md).

The [footprint report](baselines/s7-custom-delivery-report.json) retains three final-source cached
workspace samples with source SHA-256 `c7a8b64f669776b28d91f4b87f902e42e822950f3580b1820bc54c07f2249db0`. Revision is the parent;
inventory remains 272 packages and the lockfile is unchanged.

| Workspace-default metric | Median / observation | Change from idle-custom baseline |
| --- | --- | --- |
| Release CLI | 19.776 MiB | +0.691% |
| Release TUI | 19.796 MiB | +0.280% |
| Release SWE-bench | 19.904 MiB | +0.433% |
| 125-case smoke wall | 493.55 ms | +22.135% |
| Smoke peak RSS | 8,832.00 KiB | -1.429% |
| Idle offline runtime RSS | 4,556.00 KiB | +6.052% |

Wall samples: **560.65 / 395.39 / 493.55 ms**, all retained.
Default unisolated smoke/idle exclude selected custom/UI/HTTP/JS/codecs and rendered frontends.
No active-delivery RSS bound, causal speed, cold-build, allocator or section-profile claim is made.
Standalone `cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**19,667,536 bytes / 18.756 MiB**
(+0.440% against its matched preceding standalone sample), without
selected codec, QuickJS or auxiliary-model features. These are not real-model quality/billing results.

The smoke wall median rose **22.14%** from the preceding sample. Current scenario medians remain
close (SessionResume 9.53–9.85 ms versus 9.57–9.88 ms), while retained tails include one 115.68-ms
Edit and a 23.25-ms SessionResume. Case elapsed totals were 524.76 / 367.03 / 456.27 ms. These
unisolated observations neither attribute the change to this feature nor exclude a regression;
controlled profiling remains part of final release acceptance.
