# S7 native auxiliary transport-binding acceptance

TransportBinding now freezes a callback snapshot and bounded shared monitor across explicitly
created chat, System One and image-generation clients. Runtime can select it as immutable host
authority before client/registry creation. Reload/resources/package adoption retain the selected
binding while old clients stay unchanged. New config/history loads cannot reconstruct callbacks.
Arbitrary externally supplied ModelProvider objects are not secretly replaced. Core stays HTTP
independent and no new execution loop is added.

One shared driver preserves tracing/service controls, private credentials/proxy redaction,
callback/drop cleanup, bounds and observations. Native state/questions/images/assets/modalities
and experimental image opt-in remain owned semantic fields; callbacks cannot change them after
approval/admission. Native clients keep one-attempt behavior, known-zero preflight accounting,
unknown cancellation state and reported response usage before answer/image validation. Shared
diagnostics identify provider/model without recording payloads or native errors; old JSON
receives defaults for the new identity fields.

Four new provider tests exercise both actual native endpoints, immutable native inputs/flags,
private credential routes, shared failure monitoring, observation errors retaining typed usage,
preflight refusal before credential/HTTP access, cancellation child cleanup and no retries for
error/incomplete replies. Existing chat/driver tests regress the shared implementation. Three
runtime tests cover direct and Code Mode task paths, allow/deny and observer failure, actual
reload/resource retention, old-client immutability, settings/history separation, safe raw artifact
capture and exact primary/auxiliary usage subtotals. Five executable variants independently
verify those paths through real loopback HTTP/native codecs and JSONL.

Allowed variants make four requests and report 37 fixture tokens: 14 primary, 11 classifier,
12 image. Denied variants make two primary requests with zero native admissions. These are fake
credential/loopback observations, not a real model, vision-quality or billing result.

Pinned Rust 1.92.0 / Linux passed **49 final gates, 712 default workspace tests,
835 all-feature tests and 268 isolated core tests**. Standalone provider transport
retains 11 default checks and passes 15 with auxiliary support; standalone
codec/JS-free auxiliary runtime passes 2 integration checks. Prior frontend, native
image, request/interception, schema, benchmark and SWE bridge regressions passed. Commands:

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
```

These are local checks, not a hosted CI observation. See [contract](auxiliary-transport.md) and
[migration](migrations.md) for explicit binding/replacement, new protected native fields and
compatible diagnostic defaults. No dependency/default feature, package version or event/session
format changed. Actionable lifecycle hooks, expanded isolated Spark/Pi comparison and final
release acceptance remain open in the [audit](pi-parity-audit.md).

The [footprint report](baselines/s7-aux-transport-report.json) retains all three cached default
workspace samples with source SHA-256
`41c50c0ed66cf6451f9c2f85bcc8be04f85eb0a58c8f7b629a2b90f7d7f6c037`.
Revision is the parent because measurement precedes commit. Inventory remains
272 packages; locked versions and default features are unchanged.

| Workspace-default metric | Observation | Change from chat transport baseline |
| --- | --- | --- |
| Release CLI | 18.785 MiB | +0.051% |
| Release TUI | 18.905 MiB | +0.069% |
| Release SWE-bench | 19.468 MiB | +0.043% |
| 125-case smoke wall, median | 395.41 ms | -1.218% |
| Smoke peak RSS, median | 8,448.00 KiB | +1.538% |
| Idle offline runtime RSS, median | 4,100.00 KiB | -1.442% |

Wall samples are **394.72 / 395.41 / 403.97 ms**, all retained.
Unisolated default smoke/idle exclude selected callbacks, active auxiliary HTTP/JS/codecs and
rendered UIs. They do not prove causal speed changes or a hard RSS bound. No cold-build,
allocator or binary-section profile was performed. The separate standalone eval command
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**19,162,872 bytes / 18.275 MiB**
(+0.024% against its matched preceding standalone sample), with no
codec, QuickJS or auxiliary-models feature in the selected graph. It does not replace workspace
measurements or real-model quality/performance evidence.
