# S7 actionable boundary-hook acceptance

Native extensions can stage inert data or explicitly sourced context and request continuation
at successful TurnEnd/BeforeSettle boundaries. The existing executor retains model/tool history,
verifier and parent budgets; no frontend fork or implicit callback is installed. A later handler
can replace/append proposals and clear continuation. Atomic journal acknowledgement precedes
canonical projection. Restore retains facts without reinstalling callbacks or continuation.

Sixteen core tests cover order/provenance, zero-extra-request final notes, same-budget continuation,
round/token/output refusal, unsupported/failed journals, error/panic/count/byte limits, private
child cancellation/timeout, verifier rejection, paired tool results, semantic-summary source,
restoration, wire validation, follow-up/steering during settlement, primary/auxiliary metering and
post-commit elapsed-budget exhaustion. Two runtime integration tests exercise actual file tools,
allow/deny, normal/Code Mode, JSONL recovery, bounded archive windows, inert HTML and event lifecycle.
The standalone codec/JS-free runtime selects one test; Code Mode adds the second.

Four executable variants independently run that actual runtime path. Allowed variants perform
three scripted requests and account 21 fixture tokens with three native context messages.
Denied variants perform two requests/14 tokens and emit no native context, while retaining inert
denial data. Each keeps exactly one original user message; data bodies do not leak into prompts
or metadata notifications. These are offline fixture observations, not model quality or billing.

Pinned Rust 1.92.0 / Linux passed **54 final gates, 729 default workspace tests,
853 all-feature tests and 284 isolated core tests**. Prior provider/native transport,
image, request/interception, frontend, schema, benchmark and SWE bridge regressions passed.
Commands:

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
```

These are local checks, not hosted CI results. See [contract](boundary-hooks.md) and
[migration](migrations.md) for journal requirements, new exhaustive-match cases and compatible
extension wire storage. No manifests, lockfile, dependencies or default features changed.
The v1 event schema adds a metadata-only notification; typed session context retains the existing
version/Extension wire shape. Older readers retain native context as inert data, so upgraded
readers are required for prompt-equivalent branch continuation.

Pi context-edit/compaction boundary drafts, input/start/message transforms, session lifecycle
and navigation veto, expanded isolated Spark/Pi comparison and release acceptance remain open
in the [audit](pi-parity-audit.md). This feature does not claim full lifecycle parity.

The [footprint report](baselines/s7-boundary-hooks-report.json) retains three cached final-source
default workspace samples with source SHA-256 `f55fe4d7cf53472732ca754b1aea011892cfa55eb8fbafcc2161ae89d1c82a34`.
Revision is the parent because measurement precedes commit. Inventory remains
272 packages and the lockfile is unchanged.

| Workspace-default metric | Observation | Change from native transport baseline |
| --- | --- | --- |
| Release CLI | 18.998 MiB | +1.134% |
| Release TUI | 19.064 MiB | +0.843% |
| Release SWE-bench | 19.558 MiB | +0.461% |
| 125-case smoke wall, median | 392.46 ms | -0.747% |
| Smoke peak RSS, median | 8,576.00 KiB | +1.515% |
| Idle offline runtime RSS, median | 4,236.00 KiB | +3.317% |

Wall samples are **408.35 / 392.46 / 385.99 ms**, all retained.
Unisolated default smoke/idle exclude installed hooks, active auxiliary HTTP/JS/codecs and rendered
UIs. They do not prove causal speed changes or a hard RSS bound. No cold-build, allocator or
binary-section profile was performed. The separate standalone eval command
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**19,314,424 bytes / 18.420 MiB**
(+0.791% against its matched preceding standalone sample), with no
codec, QuickJS or auxiliary-models feature in the selected graph. It does not replace workspace
measurements or real-model quality/performance evidence.
