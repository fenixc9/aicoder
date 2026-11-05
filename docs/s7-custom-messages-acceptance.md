# S7 custom-message acceptance

CustomMessage retains typed source/text/images/display/details in canonical custom entries.
AgentPrompt can store idle context or trigger the existing Agent path without a synthetic User
message. Raw user hooks/expansion are skipped; startup receives explicit trigger metadata.
Core projects only the sourced content to compatible provider messages before validation,
counting and IO. Inert details and historical authority never activate automatically.

Fourteen core tests cover stateless/session triggers, append-only/restore, distinct startup and
raw-input behavior, typed wire and impersonation refusal, multimodal capabilities, body/detail
bounds, cancellation/busy leases, source context at a branch root, branch provenance, startup
cancellation, known windows, registered command sender binding, semantic summary data, control
setup mismatch and declared host prefixes resembling native labels. Three runtime integration
tests exercise actual normal/Code Mode tools, allow/deny, record-only, JSONL, bounded archive
windows, inert HTML/details and event lifecycle. The actual compatible client handles JSON/SSE
with custom images through loopback HTTP while canonical history remains Custom and details
stay out of requests. TUI display/restore and authenticated Web command integration pass.
A real PTY command creates one custom entry, one assistant reply and zero User messages, with
a registered sender and no detail exposure in the model response.

Five executable runtime variants independently exercise those paths. Normal/Code Mode allow /
deny variants retain two custom messages, zero real User messages, one startup invocation and
two scripted requests / 14 fixture tokens. Record-only appends one custom message and creates
zero model requests/startup calls/turns. These are offline fixture observations, not real-model
vision/reasoning quality or billing evidence.

Pinned Rust 1.92.0 / Linux passed **73 final gates, 791 default workspace tests,
920 all-feature tests and 335 isolated core tests**. Standalone codec/JS-free runtime
selects 2 custom integration tests; Code Mode adds the third. Provider transport passes
12 standalone and 16 all-feature checks. Prior raw-input/startup/boundary,
request/interception, native transport, images, frontends, schemas, benchmark and SWE bridge
regressions passed. Commands:

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
```

These are local checks, not hosted CI results. See [contract](custom-messages.md) and
[migration](migrations.md) for new typed prompts/triggers, canonical-only role, exhaustive-match
updates and reserved Extension storage. Existing user APIs remain compatible. No manifests,
lockfile, dependencies or default features changed, and no session/event version is bumped.
Display intent affects UI, not model inclusion; explicit exports retain inert details.

Busy custom steer/follow-up/next-turn and deferred append, message transforms, session lifecycle /
navigation veto, boundary context-edit/compaction drafts, expanded isolated Spark/Pi comparison
and release acceptance remain open in the [audit](pi-parity-audit.md). This increment verifies
idle storage/trigger paths and does not claim full custom-message lifecycle parity.

The [footprint report](baselines/s7-custom-messages-report.json) retains three cached final-source
default workspace samples with source SHA-256 `f8d5c5b9d4a10954de0109a91c2748925a9e6b053a96b7499943054b874f8617`.
Revision is the parent because measurement precedes commit. Inventory remains
272 packages and the lockfile is unchanged.

| Workspace-default metric | Observation | Change from raw-input baseline |
| --- | --- | --- |
| Release CLI | 19.640 MiB | +0.314% |
| Release TUI | 19.741 MiB | +0.290% |
| Release SWE-bench | 19.818 MiB | +0.003% |
| 125-case smoke wall, median | 404.10 ms | +0.269% |
| Smoke peak RSS, median | 8,960.00 KiB | +2.941% |
| Idle offline runtime RSS, median | 4,296.00 KiB | +1.416% |

Wall samples are **395.29 / 1156.19 / 404.10 ms**, all retained.
Unisolated default smoke/idle exclude selected custom/UI paths, active HTTP/JS/codecs and rendered
UIs. They do not profile active custom messages or prove causal speed changes or hard RSS bounds.
No cold-build, allocator or binary-section profile was performed. Separate
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**19,581,320 bytes / 18.674 MiB**
(+0.007% against its matched preceding standalone sample), with no
codec, QuickJS or auxiliary-models feature in the selected graph. These observations do not
replace workspace measurements or real-model quality/performance evidence.
