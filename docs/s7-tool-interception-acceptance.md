# S7 native tool-interception acceptance

Native ToolCallHook and ToolResultHook now provide ordered typed argument and delivered-result
transformations over the existing dispatcher. Argument edits precede legacy guards, owned
preparation and approval; immutable source calls remain canonical with inert proposal records.
Result callbacks chain bounded delivered envelopes while actual execution/completion/capture
status remains explicit. Their artifact exposure can only shrink. Unknown/unadvertised calls
reject before interception, and denied approval is never converted into execution by a result
callback. No native code, model route or grant is restored from records.

ResultPending precedes execution under a supporting journal. Approval revocation and steering
are rechecked after that awaited receipt. Receipt writes and callback phases are bounded and
cancellation-aware; callback children expire on timeout/drop. Missing journal support fails
before external effects. Invalid/failed/timed-out result transforms preserve completion facts
and explicit incomplete delivery without repeating an operation. Unknown completion is null,
never an invented false. Final receipts describe interception; later parent truncation or
checkpoint/event delivery can still fail independently.

Ten new core tests cover chained priority/tie order, source/effective calls, guards and exact
owned approval, changed delivered status with immutable original execution facts, unsupported
journals, forged/altered/restored artifact refusal, filtering, native failures and byte limits,
hidden tool rejection, timeout child cleanup, parent cancellation after effects, revoke/cancel
during Pending persistence, inert resume, known/unknown completion before details reduction and
unchanged timeout event categories. Two runtime tests use actual
write_file/read_file plans, JSONL sessions and finalized tool events, including Code Mode
leaves and approval denial. Four runnable example variants independently verify allowed/denied
normal/program dispatch, normalized approved targets, unchanged source files and redacted
new output/history/model requests. Earlier raw archives are not erased by delivery redaction.

Pinned Rust 1.92.0 / Linux passed **35 final gates, 686 default workspace tests,
802 all-feature tests and 255 isolated core tests**. Standalone codec/JS-free
runtime interception passed 1 test; preceding acquisition/classifier/image paths and
actual frontend/protocol/benchmark regressions also passed. Commands:

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
```

These are local results, not a hosted CI run. Native callbacks require explicit SDK/compiled
host registration; settings/history cannot load code. No new stock frontend loader or real
Spark/Pi model test is claimed. The [contract](tool-interception.md) documents actual phase and
redaction/capture boundaries; [migration](migrations.md) covers additive traits/registrations,
custom journals and generic tool-envelope metadata. Session/event versions and dependencies
remain unchanged. Provider/request transformations, additional actionable lifecycle hooks,
expanded isolated comparisons and release remain open in the [Pi audit](pi-parity-audit.md).

The [footprint report](baselines/s7-tool-interception-report.json) retains all three cached
default workspace samples with source SHA-256
`17fa114015157b57fd0e20ca1f115fe03fb92256303fc25d31dde2cb443b091c`.
Revision is the parent because measurement precedes commit. Package inventory remains
272; no package versions or default features changed.

| Workspace-default metric | Observation | Change from image-generation baseline |
| --- | --- | --- |
| Release CLI | 18.433 MiB | +2.139% |
| Release TUI | 18.408 MiB | +1.826% |
| Release SWE-bench | 19.122 MiB | +1.451% |
| 125-case smoke wall, median | 396.30 ms | +1.491% |
| Smoke peak RSS, median | 8,320.00 KiB | +3.175% |
| Idle offline runtime RSS, median | 4,228.00 KiB | +2.027% |

Wall samples are **396.30 / 386.41 / 942.30 ms**, all retained.
These unisolated observations exclude selected interception callbacks, active JS/codecs/native
HTTP and rendered UIs; they establish neither causal speed changes nor a hard RSS bound.
No cold-build, allocator or binary-section profile was performed. The separate standalone eval
command `cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**18,806,352 bytes / 17.935 MiB**
(+1.534% against its matched preceding standalone sample), with
no codec, QuickJS or auxiliary-models feature in its selected dependency tree. It does not
replace workspace-unified measurements or real-model quality/performance evidence.
