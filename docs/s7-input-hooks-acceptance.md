# S7 raw-input hook acceptance

Native input hooks continue, transform or consume raw user input before expansion, persistence
or steering notification. Core owns bounded ordered reservations and ephemeral recovery; runtime
expands the first prompt block from its frozen resource snapshot. Safe receipts contain source,
size and change metadata, never the original text/images. Effective user content is canonical.
Existing branch facts, provider routes and tool/approval authority remain unchanged.

Twenty-two core tests cover raw/expansion order and attachments, source spoof refusal, handled
initial inputs without fabricated answers/turns, handled controls during model/tool execution,
actual transformed steering/follow-up and usage, bounded reservations/queue payloads, FIFO under
reverse polling, unpolled/drop/cancel/timeout cleanup, late closure, invalid/error/panic/oversize
failure, prepared owner/delivery markers, cached and pre-owner elapsed budgets, waiting deadlines,
failed input persistence with ephemeral SDK originals, no raw credential body in receipts, whole
output admission and known windows before session writes. Three runtime tests exercise actual
normal/Code Mode file tools, allow/deny, raw rewrite into a real template, consumed unknown
commands, JSONL, bounded archives, inert export and event lifecycle.

Two RPC tests verify explicit handled reports and status/cancel availability while an input hook
waits. Two TUI adapter tests retain drafts until acknowledgement and preserve newer edits. An
authenticated Web integration verifies transform-before-template and handled results. A real PTY
runs native input UI both before model startup and during an active request, checks bracketed
paste as data, one original model completion, zero extra user messages and no raw bodies on disk.

Five executable runtime variants independently exercise the actual paths. The four normal /
Code Mode allow/deny variants invoke raw preparation once, make two scripted requests and report
14 fixture tokens, with one effective User message. The handled variant makes zero requests,
creates no User/assistant/turn record and does not execute a tool. These are offline fixture
observations, not actual model quality or billing.

Pinned Rust 1.92.0 / Linux passed **66 final gates, 773 default workspace tests,
900 all-feature tests and 321 isolated core tests**. Standalone codec/JS-free runtime
selects 2 input integration tests; Code Mode adds the third. Prior startup/boundary,
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
cargo test --locked -p aicoder-runtime --no-default-features --test input_hooks
cargo run --locked -p aicoder-runtime --example offline_input_hooks
cargo run --locked -p aicoder-runtime --example offline_input_hooks -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_input_hooks -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_input_hooks -- --code-mode --deny
cargo run --locked -p aicoder-runtime --example offline_input_hooks -- --handled
python3 scripts/check_input_hooks.py
```

These are local checks, not hosted CI results. See [contract](input-hooks.md) and
[migration](migrations.md) for raw Agent prompts, eager reservation futures, async replies,
new RunStop/RunInput Rust cases and custom journal requirements. No manifests, lockfile,
dependencies or default features changed. V1 event/RPC schemas add the handled stop/status;
sessions retain their existing version and generic extension storage. History cannot restore
private originals, prepared callbacks, routing or grants.

Message transforms, idle custom-message session submission, session lifecycle/navigation veto,
boundary context-edit/compaction drafts, expanded isolated Spark/Pi comparison and release
acceptance remain open in the [audit](pi-parity-audit.md). Stock Web control routes still focus
on cancellation/approval; the reusable core exposes the ordered input API to Web hosts.

The [footprint report](baselines/s7-input-hooks-report.json) retains three cached final-source
default workspace samples with source SHA-256 `0f852244d4c981b39333aae63749b9d1cd467d1d19543defa6a6fcc3ca489eb9`.
Revision is the parent because measurement precedes commit. Inventory remains
272 packages and the lockfile is unchanged.

| Workspace-default metric | Observation | Change from startup-hook baseline |
| --- | --- | --- |
| Release CLI | 19.579 MiB | +1.998% |
| Release TUI | 19.684 MiB | +2.176% |
| Release SWE-bench | 19.818 MiB | +0.838% |
| 125-case smoke wall, median | 403.02 ms | +1.573% |
| Smoke peak RSS, median | 8,704.00 KiB | +0.000% |
| Idle offline runtime RSS, median | 4,236.00 KiB | -1.397% |

Wall samples are **397.78 / 810.94 / 403.02 ms**, all retained.
Unisolated default smoke/idle exclude explicitly installed raw hooks, input UI, active HTTP /
JS/codecs and rendered UIs. They do not profile active hooks or prove causal speed changes or a
hard RSS bound. No cold-build, allocator or binary-section profile was performed. Separate
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**19,579,968 bytes / 18.673 MiB**
(+0.838% against its matched preceding standalone sample), with no
codec, QuickJS or auxiliary-models feature in the selected graph. These observations do not
replace workspace measurements or real-model quality/performance evidence.
