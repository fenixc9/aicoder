# S7 Code Mode only and root catalog acceptance

Code Mode only now advertises codemode alone to the outer model while preserving the
separately allowed leaf registry. CLI/TUI/Web expose --code-mode-only; RPC inherits its CLI
host. Settings/environment support code_mode.only with normal field precedence and require
enabled Code Mode. Default normal exposure and disabled Code Mode remain compatible.

Core adds immutable host root model catalogs and applies them consistently to preparation,
model selection, context/token measurement, compaction and fresh root dispatch. Registered
but unadvertised model calls return a paired tool_not_exposed result before preparation,
guards or approval-cache lookup. Unknown names retain unknown_tool. Per-request subsets
receive the same protection in normal mode. Host tools remain installed and composition
uses its own catalog/guard/budget path. Canonical historical calls/results retain their
provenance without granting fresh direct invocation rights.

Six new tests cover core selection validation and empty/subset catalogs, unadvertised
registered mutation attempts, zero preparation/execution, paired canonical failures without
recovery uncertainty, explicit request subsets, settings precedence/contradictions, root/leaf
registry separation and benchmark provenance compatibility. The preceding six Code Mode image
tests now run their runtime cases with only enabled, retaining actual allowed leaf/image
execution, guards, limits and cancellation. The core tests use no engine/codec/HTTP.

The actual CLI JSON, RPC, authenticated Web and TUI PTY driver uses the frontend only flag.
Every model request advertises exactly codemode; script read_file/image_output calls still
execute normally. It retains non-vision, no-storage and filtered-image outcomes, plus a
malicious direct write_file response: the host records its refusal and never creates the
sentinel file, even with CLI --yes. Eight cases retain two loopback requests each, sixteen
total. Source images stay unchanged and canonical sessions acquire no synthetic user turns.

Pinned Rust 1.92.0 / Linux passed **26 final gates, 671 default workspace tests, 772 all-feature
tests and 241 isolated core tests**. Standalone codec-free runtime retains 32 acquisition/library
checks and six opt-in Code Mode image checks. Commands:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked -p aicoder-runtime --no-default-features --test image_files --lib
cargo test --locked -p aicoder-runtime --no-default-features --features code-mode --test code_mode_images
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
```

CI includes the only driver; these are local observations, not a hosted CI run. No actual
Spark/Pi generation, vision-quality or billed-cost experiment was performed. only is root
exposure, not an authorization grant or read-only sandbox. Leaf schemas/permissions remain
host-selected, and denied attempts consume ordinary parent admission budgets. Root selections
validate against the installed immutable registry; an empty selection cannot fall back to all.

Benchmark provenance retains installed tools and optionally records a narrower model_tools
catalog. Older observations default that field to absent; replay cannot restore authority
from it. No persisted session, dependency or default feature changes were made. Public Rust
literal/enum compatibility and flags are covered in [migration](migrations.md). Classifier/
image-model helpers, interception breadth, final isolated evaluation and release remain open
in the [Pi audit](pi-parity-audit.md). See [root catalog contract](code-mode.md#only-mode-and-root-declarations).

The [footprint record](baselines/s7-code-only-report.json) retains three cached workspace-default
samples with source SHA-256
`fe3189fcc27aa51f6667344fb26c64de4560a5d817dcb3acdb6a180cbe35a7f0`.
Revision is the parent because measurement precedes commit. Resolved inventory remains
272 packages and lockfile versions are unchanged.

| Workspace-default metric | Observation | Change from explicit image baseline |
| --- | --- | --- |
| Release CLI | 17.685 MiB | -0.024% |
| Release TUI | 17.755 MiB | -0.112% |
| Release SWE-bench | 18.499 MiB | +0.147% |
| 125-case smoke wall time, median | 387.23 ms | +1.711% |
| Smoke peak RSS, median | 7,808 KiB | -1.613% |
| Idle offline runtime RSS, median | 4,220 KiB | +3.839% |

Wall samples are **379.79 / 396.19 / 387.23 ms**, all retained. These unisolated default
observations exclude active JS/image work and rendered frontends; they do not measure only-mode
service latency, causal improvements/regressions or a hard RSS bound. No cold build, allocator
or binary-section profile was performed. Standalone eval remains an explicitly separate
codec/JS-free feature graph: `cargo build --locked --release -p aicoder-eval --bin aicoder-swebench`
produced **18,171,064 bytes / 17.329 MiB**, +0.293% against the preceding matched standalone
observation. It does not replace the larger workspace result or a real-model comparison.
