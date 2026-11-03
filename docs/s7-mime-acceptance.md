# S7 bounded image MIME compatibility acceptance

Image MIME interpretation now agrees across core user/model boundaries, artifact promotion,
optional runtime decoding and compatible-provider encoding. Case and parameters are accepted;
image/jpg maps to image/jpeg and BMP aliases map to image/bmp. Original declarations,
immutable references, pixels and canonical messages remain unchanged. Only codec expectations,
resolved previews and provider data URI headers use normalized types. Unknown valid image
subtypes retain explicit unsupported-preview handling; controls/non-image declarations fail.
No dependencies, default features or persisted formats changed. See [contract](tool-images.md#mime-declarations).

Six new tests cover bounded MIME interpretation, alias/header views, invalid/control/injected
MIME, unchanged AgentInput, actual JPEG/BMP decoding and mismatches, unknown image types and
completed MCP image calls whose declarations are not images. The existing real MCP-to-HTTP
parallel/replay test now runs both canonical and mixed-case/parameter declarations, retaining
raw MIME in JSONL and decoding prepared JPEG wire parts. Its source-page identity checks still
compare the exact immutable reference; interpretation does not relax storage binding.

The actual CLI/RPC/Web/TUI PTY image driver also runs with the aliased MCP peer. It verifies
image delivery, original reference declarations, no synthetic user history/private metadata,
explicit approvals and child cleanup. Invalid image MIME produces a capture error with
operation_completed true and incomplete delivery; it does not create a forged image reference
or automatically repeat the call. Parameters are never fetched or used as authorization.

Pinned Rust 1.92.0 / Linux passed **22 gates, 664 default workspace tests, 758 all-feature tests,
236 isolated core tests and 32 standalone codec-free runtime tests**:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked -p aicoder-runtime --no-default-features --test image_files --lib
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
```

CI includes the alias driver; observations establish local checks, not hosted CI execution.
The alias driver uses eight loopback requests with fake credentials. No real Spark/Pi,
vision-quality or billed-cost experiment was performed. Pixel/source/frame budgets retain
the preceding image contracts, and default core/runtime still require no codecs. Code Mode
explicit image output/selection, auxiliary models, only mode, interception and larger isolated
real-service evaluation remain open in the [Pi audit](pi-parity-audit.md).

The [footprint record](baselines/s7-mime-report.json) retains three cached workspace-default
samples with source SHA-256
`4170f71575d1eecd2037a31209a06292c9266fc612c3c279c7f4715a99ee7e24`.
Revision is the parent because measurement precedes commit; resolved inventory remains 272
packages and no lockfile versions changed.

| Workspace-default metric | Observation | Change from native image baseline |
| --- | --- | --- |
| Release CLI | 17.669 MiB | +0.183% |
| Release TUI | 17.755 MiB | +0.191% |
| Release SWE-bench | 18.453 MiB | +0.201% |
| 125-case smoke wall time, median | 395.19 ms | +3.867% |
| Smoke peak RSS, median | 8,064 KiB | +1.613% |
| Idle offline runtime RSS, median | 4,040 KiB | -2.791% |

Wall samples are **492.30 / 392.10 / 395.19 ms**, all retained. These unisolated cached
observations establish no latency/memory improvement or causal regression attribution.
They exclude active decoding and rendered frontends, cold builds, allocator and binary-section
profiling. Standalone eval remains a separate codec-free feature graph:
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**18,097,656 bytes / 17.259 MiB**, +0.045% against the preceding matched standalone observation,
with no image crate/features in its selected tree. It does not replace the workspace result.
