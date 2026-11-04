# S7 native auxiliary classifier acceptance

Native classifiers now execute bounded typed bool/choice/score tasks through explicit trusted
host routes and the System One /systemone protocol. Bool maps to native noul; there is no
chat emulation or second agent loop. SDK prepared operations and Code Mode models helpers use
the same parent-run admission, approval, cancellation and journal path. Image generation is
still pending, as are broader interception hooks and final isolated real-model comparisons.

Core owns the classifier contracts, immutable auxiliary-operation interface and per-run
metering. Providers own HTTP and request-time credential resolution; runtime owns trusted
profiles/catalogs and prepared tools. Project settings can select trusted profile names but
cannot define routes. Default builds gain no dependency or service client; optional
auxiliary-models works without JS and optional code-mode includes it.

Task requests reserve estimated input and output allowance atomically against the parent,
respect known model windows, use at most four concurrent slots and admit at most 128 attempts
or a smaller configured ceiling. Durable Started precedes HTTP. Settlement retains service
usage before invalid-answer detection; interrupted/unknown work charges its estimate with
explicit uncertainty. Reservations release on error/drop, mixed usage preserves primary and
per-task subtotals, and TUI never prices all work at the primary rate. No implicit retries or
redirects occur, including after completed inference with failed delivery. Model metadata,
session records and script-supplied identities cannot create routing/approval authority.

Focused tests cover native wire bool/choice/score, success and malformed answers with reported
usage, Code Mode catalog/helper execution, eight concurrent tasks bounded to four requests,
atomic parent reservations, zero-task budgets, cancellation with unknown usage, unsupported
journals before HTTP, snapshots/project routing boundaries and three explicit image protocols.
The actual CLI JSON, RPC, authenticated Web and TUI PTY driver exercises only-mode catalogs,
independent leaf approval, typed classifier output and canonical Started/Finished records.
It makes eight loopback main-model requests and four native-task requests, no real model calls.

Pinned Rust 1.92.0 / Linux passed **28 final gates, 673 default workspace tests, 782 all-feature
tests and 243 isolated core tests**. Codec-free standalone runtime passed 32 acquisition/library,
six Code Mode image and five auxiliary-model tests. These are local observations, not a hosted
CI result. Commands:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked -p aicoder-runtime --no-default-features --test image_files --lib
cargo test --locked -p aicoder-runtime --no-default-features --features code-mode --test code_mode_images
cargo test --locked -p aicoder-runtime --no-default-features --features auxiliary-models --test auxiliary_models
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
```

The first all-feature run encountered a preceding Code Mode image test returning one image
instead of two. Its original failure is retained locally. Assertion diagnostics were added;
eight independent executions of that test binary and the complete final gate run passed.
The cause remains unconfirmed; this record does not claim a storage-timeout diagnosis or a
causal fix. No real classifier credential, Spark/Pi generation or image-quality test was used.

[Migration](migrations.md) describes additive public Rust fields/hooks, exhaustive enum changes,
custom journal support and optional event/RPC breakdowns. Session-v3 Extension records retain
canonical indices and grant no replay authority. [Classifier contract](auxiliary-models.md)
documents configuration, SDK and models helpers. The [Pi audit](pi-parity-audit.md) retains
image generation, broader hooks, expanded isolated evaluation and release as open work.

The [footprint record](baselines/s7-classifiers-report.json) retains all three cached default
workspace samples with source SHA-256
`098b784670844a8a7674a24af292863fb44381b8404c9ddeaedae2c741b6fa72`.
Revision names the parent because measurement precedes commit. Resolved inventory remains
272 packages and lockfile versions are unchanged.

| Workspace-default metric | Observation | Change from only-mode baseline |
| --- | --- | --- |
| Release CLI | 18.001 MiB | +1.788% |
| Release TUI | 18.031 MiB | +1.555% |
| Release SWE-bench | 18.813 MiB | +1.692% |
| 125-case smoke wall, median | 396.98 ms | +2.519% |
| Smoke peak RSS, median | 8,192.00 KiB | +4.918% |
| Idle offline runtime RSS, median | 4,196.00 KiB | -0.569% |

Wall samples are **396.98 / 385.97 / 620.28 ms**, including the last outlier.
These unisolated samples exclude active native tasks, JS/image processing and rendered UIs;
they do not establish causal speed changes or a hard memory bound. No cold build, allocator
or binary-section profiling was performed. The separate standalone eval build
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**18,494,712 bytes / 17.638 MiB**
(+1.781% against its matched prior observation), with no image,
QuickJS or auxiliary-models feature in its selected dependency tree. It does not replace
the larger workspace-unified measurement or real-model evaluation.
