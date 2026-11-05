# S7 startup-hook acceptance

Native extensions can prepare a run's System prefix, admitted root-tool selection and sourced
context before turn validation/persistence and request-local views. The original user message
and recorded branch facts remain intact. Prefix overrides expire on the next turn, including
on the same Agent. Empty root catalogs remain empty; dispatch still rejects fresh hidden calls
before preparation and preserves Code Mode leaf approvals.

Fifteen core tests cover startup ordering and proposed context, root reselection/empty catalogs,
original input and privacy, initial session persistence and restore, invalid roles/payloads/
counts, unknown tools, error/panic sanitation, output admission, request-view ordering and once-only
preparation, model-selection previews and executor mismatch, unsupported/failed journals,
acknowledged facts after deadline, cancellation/timeout child cleanup, steering while startup
waits, old receipt compatibility and next-turn expiry. Two runtime integration tests exercise
actual normal/Code Mode tools, allow/deny, JSONL, bounded archives, inert HTML and event lifecycle.

Four executable variants independently run the actual runtime path. Each calls startup once,
makes two scripted requests and accounts 14 fixture tokens with one native context message.
Each keeps one literal original user message; inert metadata does not enter prompts, and private
prefix text does not enter session receipts or metadata notifications. Denied mutations never
create the output file. These are offline observations, not real model quality or billing.

Pinned Rust 1.92.0 / Linux passed **59 final gates, 745 default workspace tests,
870 all-feature tests and 299 isolated core tests**. Standalone codec/JS-free runtime
selects 1 startup integration test; Code Mode adds the second. Existing boundary,
request/interception, native/provider transport, images, frontends, schemas, benchmark and SWE
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
```

These are local checks, not hosted CI results. See [contract](startup-hooks.md) and
[migration](migrations.md) for for_run preparation, journal requirements and new Rust enum/field
cases. No manifests, lockfile, dependencies or default features changed. BoundaryCommit.startup
has a serde default and omits None for older round receipts. BeforeStart extends the v1 event
boundary discriminator; no session/event format version is bumped.

Raw-input continue/transform/handled interception, idle custom-message session submission, message
transforms, session lifecycle/navigation
veto, boundary context-edit/compaction drafts, expanded isolated Spark/Pi comparison and release
acceptance remain open in the [audit](pi-parity-audit.md). Startup does not implement a late
input-consumption fallback that would first interrupt an already active model request.

The [footprint report](baselines/s7-startup-hooks-report.json) retains three cached final-source
default workspace samples with source SHA-256 `285a17575748c7629058b1cdb98752d8317ad3f9fd53b579aad0a29bcffa52cd`.
Revision is the parent because measurement precedes commit. Inventory remains
272 packages and the lockfile is unchanged.

| Workspace-default metric | Observation | Change from boundary-hook baseline |
| --- | --- | --- |
| Release CLI | 19.195 MiB | +1.041% |
| Release TUI | 19.265 MiB | +1.053% |
| Release SWE-bench | 19.653 MiB | +0.486% |
| 125-case smoke wall, median | 396.78 ms | +1.101% |
| Smoke peak RSS, median | 8,704.00 KiB | +1.493% |
| Idle offline runtime RSS, median | 4,296.00 KiB | +1.416% |

Wall samples are **396.78 / 394.58 / 406.70 ms**, all retained.
Unisolated default smoke/idle exclude installed hooks, active auxiliary HTTP/JS/codecs and rendered
UIs. They do not prove causal speed changes or a hard RSS bound. No cold-build, allocator or
binary-section profile was performed. The separate standalone eval command
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**19,417,160 bytes / 18.518 MiB**
(+0.532% against its matched preceding standalone sample), with no
codec, QuickJS or auxiliary-models feature in the selected graph. It does not replace workspace
measurements or real-model quality/performance evidence.
