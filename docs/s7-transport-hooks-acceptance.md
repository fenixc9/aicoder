# S7 compatible chat transport-hook acceptance

ChatClient now binds explicit immutable native headers/payload/response/parsed-stream stages.
Safe application headers and host-allowlisted service controls prepare once, freeze and reuse
across configured attempts; private bearer credentials refresh separately. Original adapter
fields, core semantic aliases, framing and auth/routing headers cannot be changed. Core prompt/
option/tool edits remain in counted request views. Model-service HTTP contracts stay in providers.

Response metadata and whole parsed JSON SSE views are read-only, bounded and credential-redacted.
Observer error/panic/timeout does not alter normalization or cause retries. Known usage publishes
before observer waits, surviving cancellation and deduplicating final snapshots. Oversized or
unrepresentable observation values are omitted explicitly; the original typed codec determines
validity. Preparation errors report known zero model usage before HTTP. Private callback children
expire on drop, and parent deadlines can be observed after synchronous native preparation or
credential work before new attempts. Hooked successful-header body failures are not retried.

Eleven new provider tests cover actual JSON/SSE header/payload ordering, deletion and protected
fields, safe observation/redaction, error/panic isolation, bounded diagnostics, omissions without
model truncation, unchanged normalization for ignored huge JSON numbers, known usage before
cancelled observation, preflight zero usage/no credential or HTTP access, immutable retries,
success-header delivery uncertainty, public driver sessions and parent duration admission.
One runtime test and two executable variants compose real codecs, core request views,
SessionModelClient/for_model and JSONL history. They verify tracing identity, allowed metadata,
final options/empty root tools, private header filtering, safe receipts and usage once. Tests use
loopback/fake credentials; no real generation or quality/billing measurement is claimed.

Pinned Rust 1.92.0 / Linux passed **42 final gates, 712 default workspace tests,
828 all-feature tests and 268 isolated core tests**. Standalone provider transport
passed 11 tests; codec/JS-free standalone runtime transport passed 1 test
covering JSON/SSE. Existing frontend, image/auxiliary, request/interception, schema, benchmark
and SWE bridge regressions also passed. Commands:

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
```

These are local checks, not a hosted CI result. The binding currently covers compatible chat
and session-model clients. Native classifier/image transport binding remains separate work;
TransportSession is reusable but does not silently install hooks into them. Actionable lifecycle
hooks, expanded isolated Spark/Pi comparison and release remain open in the [audit](pi-parity-audit.md).
[Contract](transport-hooks.md) and [migration](migrations.md) describe cooperative/native-code
limits, observer view precision, explicit client assembly, the opted-in body retry change and
unchanged default behavior. No dependency/default feature or event/session version changed.

The [footprint report](baselines/s7-transport-hooks-report.json) retains all three cached default
workspace samples with source SHA-256
`e8dc2861f4d18989face8f1c0b93e345ee8de5bd6100a300cddf402468b3c4e4`.
Revision is the parent because measurement precedes commit. Inventory remains
272 packages; locked versions and default features are unchanged.

| Workspace-default metric | Observation | Change from request-view baseline |
| --- | --- | --- |
| Release CLI | 18.775 MiB | +0.996% |
| Release TUI | 18.891 MiB | +0.980% |
| Release SWE-bench | 19.460 MiB | +0.961% |
| 125-case smoke wall, median | 400.29 ms | +0.690% |
| Smoke peak RSS, median | 8,320.00 KiB | -2.985% |
| Idle offline runtime RSS, median | 4,160.00 KiB | +0.580% |

Wall samples are **404.00 / 400.29 / 396.30 ms**, all retained.
Unisolated default smoke/idle exclude selected callbacks, active HTTP/JS/codecs and rendered
frontends. They do not prove causal speed changes or a hard RSS bound. No cold-build, allocator
or binary-section profile was performed. The separate standalone eval command
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**19,158,240 bytes / 18.271 MiB**
(+1.014% against its matched preceding standalone sample), with no
codec, QuickJS or auxiliary-models feature in the selected graph. It does not replace workspace
measurements or real-model quality/performance evidence.
