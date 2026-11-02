# S7 packaged palette acceptance

TUI startup --theme-package PACKAGE:PATH and /theme PACKAGE:PATH now use declared installed
package themes directly. Runtime returns immutable verified asset bytes and content-pin
metadata; frontend palette parsing stays in TUI. Explicit selection works without automatic
resource discovery/global enablement while preserving the host package policy. Selection
does not install code, resolve credentials, create missing storage or call a model.

Four runtime tests cover declared resource kind, byte/path bounds, host disabling, explicit
selection of a globally disabled root, immutable captures after tampering/removal and reading
without executing bundled code. The actual PTY driver observes the initial and changed RGB
colors, rejects undeclared/changed assets, verifies the package program never ran and observes
zero HTTP model requests. Invalid selections leave the current palette intact. Colors remain
frontend state and change only through explicit selection, including after resource reload.

Pinned Rust 1.92.0 / Linux checks passed:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-runtime -p aicoder-eval --features server,host-extensions,mcp-fixtures --bins --examples
python3 scripts/check_package_themes.py
python3 scripts/check_tui_interaction.py
python3 scripts/check_host_extensions.py --packages
python3 scripts/check_frontend_parity.py
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
```

All 16 final gates passed: **642 default workspace, 716 all-feature and 226 isolated core
tests**. Existing terminal controls, executable package lifecycle/approvals, normalized
SDK/CLI/RPC/Web flow and benchmark/SWE contracts stay green. CI includes the new PTY driver;
local verification does not claim hosted CI execution. No real model credentials or remote
service calls were used. Session/package formats and dependency/default-feature sets are
unchanged.

The [Pi matrix](pi-parity-audit.md) now records executable and palette package activation as
implemented stock paths. Remote distribution, typed visible tool media, additional Code Mode
helpers and hook coverage remain separate work; this is not full Pi/release acceptance.

The [default footprint record](baselines/s7-package-theme-report.json) retains all three
final-source samples. Their source SHA-256 is
`ed1a780ab5553060e4029dc49b559218ee54cd0d33c822e54584274db603ef4d`;
the revision is the parent because collection precedes commit. Resolved packages remain 272.

| Default metric | Observation | Change from package executable hosts |
| --- | --- | --- |
| Release CLI | 17.445 MiB | +0.065% |
| Release TUI | 17.543 MiB | +0.112% |
| Release SWE-bench | 17.266 MiB | +0.079% |
| 125-case smoke wall time, median | 390.60 ms | -0.961% |
| Smoke process peak RSS, median | 7,680 KiB | -1.639% |
| Idle offline runtime RSS, median | 4,232 KiB | +0.189% |

Wall samples are 403.39 / 390.60 / 389.46 ms, all retained. Cached default smoke/idle
observations do not measure rendered TUI/package loading or guarantee latency. Explicit
palette reads remain bounded; no additional model/Pi quality experiment was performed.
