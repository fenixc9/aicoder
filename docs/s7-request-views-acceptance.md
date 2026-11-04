# S7 provider-neutral request-view acceptance

RequestHook now transforms ephemeral conversation/system views, generation options and root
tool subsets before token admission. Explicit scope preserves or replaces the declared runtime
prefix; complete tool units, capabilities, adapter compatibility, known windows and parent
budgets are checked. Model identity/routing, stream mode and host definition schemas remain
immutable. Empty tool subsets stay empty at actual dispatch and compatible HTTP encoding.

Session/model preparation freezes the first view before mutation. Applied controls invalidate
it; executor identities prevent cross-host reuse. Each later round starts from base settings,
not previous edits. Request-only effort never becomes a branch choice. Compaction summarizes
raw context/history, commits branch checkpoints, then rebuilds the hooked view. Final materialized
views are recounted/revalidated and generation allowance remains parent clamped. Metadata-only
Admission/Ready receipts record intent and safe effective options without persisting view text,
stop strings or schemas. Missing optional journal support fails before associated provider IO.

Thirteen new core tests cover pre-admission filtering, explicit prefix scope, actual dispatch of
narrowed catalogs, request-local options and frozen first preparation, invalid capabilities/
units/window inflation/parent budgets, supporting journals before summary IO, raw semantic
summaries with committed checkpoints before the main request, compatible model switches,
inert restore, callback timeout/cancellation cleanup, queued controls, executor cache ownership,
stable ordering, private-option omissions and elapsed native/journal deadline boundaries.
A runtime test and two runnable variants use actual JSON/SSE HTTP adapters and JSONL sessions,
checking transformed prompt/temperature/seed/max_tokens/empty tools, safe receipts, unchanged
canonical input, no credential/private-view persistence and usage counted once. These are
loopback fixtures with fake credentials, not real model/quality experiments.

Pinned Rust 1.92.0 / Linux passed **38 final gates, 700 default workspace tests,
816 all-feature tests and 268 isolated core tests**. Standalone codec/JS-free
runtime request views passed 1 test covering JSON and SSE. Existing actual frontend,
image/auxiliary/interception, schema, benchmark and SWE bridge regressions also passed. Commands:

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
```

During final review, a targeted test reproduced provider invocation after synchronous Ready
receipt work crossed the total duration. Final code captures elapsed-budget checks after
callback/journal work and before summary/main invocation. Both regression branches pass in
the final suite. Preliminary earlier-source checks are retained locally and do not replace
this final-source run. These are local results, not a hosted CI observation.

The [contract](request-views.md) documents callback/compaction passes, native-code/cooperative
limits, complete/incomplete scalar observations and no replay authority. [Migration](migrations.md)
covers new optional journal support and async preparation before model compatibility checks
when request hooks are installed. There are no new dependencies/default features or session/
event versions. Raw HTTP headers/payloads, transport observations, actionable lifecycle hooks,
expanded isolated Spark/Pi comparison and release remain open in the [audit](pi-parity-audit.md).

The [footprint report](baselines/s7-request-views-report.json) retains all three cached default
workspace samples with source SHA-256
`ba3b8d2eefaec06f159b6198daa2796c9eff310d1b578d4a02e6378dbab2c631`.
Revision names the parent because measurement precedes commit. The inventory remains
272 packages; no locked package versions or default features changed.

| Workspace-default metric | Observation | Change from tool-interception baseline |
| --- | --- | --- |
| Release CLI | 18.590 MiB | +0.851% |
| Release TUI | 18.708 MiB | +1.633% |
| Release SWE-bench | 19.275 MiB | +0.802% |
| 125-case smoke wall, median | 397.55 ms | +0.314% |
| Smoke peak RSS, median | 8,576.00 KiB | +3.077% |
| Idle offline runtime RSS, median | 4,136.00 KiB | -2.176% |

Wall samples are **397.55 / 390.64 / 397.64 ms**, all retained.
These unisolated observations exclude selected request callbacks, active native HTTP/JS/codecs
and rendered frontends. They do not prove causal speed changes or a hard RSS bound. No cold-build,
allocator or binary-section profile was performed. The separate standalone eval command
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**18,965,872 bytes / 18.087 MiB**
(+0.848% against the matched preceding standalone sample), with no
codec, QuickJS or auxiliary-models feature in its selected graph. It does not replace workspace
measurements or real-model quality/performance evidence.
