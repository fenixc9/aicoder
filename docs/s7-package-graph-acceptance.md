# S7 atomic remote dependency closure acceptance

[Host source catalogues](remote-packages.md) now acquire complete native exact-pin closures
for Git HTTPS/npm root installs. Root and dependencies prepare private immutable payloads;
only one revision-checked registry publication installs/selects the result. Source catalogues
cannot define credentials, programs or private paths. Existing trusted pins reuse their
original consent, and existing active dependency selections are preserved. Only the requested
root is enabled. Local imports keep their serialized copy/publication behavior.

Seven new local-service tests cover a real three-package transitive closure, missing/wrong
source pins, aggregate byte limits, installed old-pin reuse without transfer/upgrade,
concurrent registry edits, bounded/duplicate/credential-routing catalogue data and cancellation
after root preparation. The actual CLI driver verifies one registry revision for the complete
three-package install, root-only enablement, failed-pin isolation, concurrent writer preservation
and cleaned staging. All tests use loopback fixtures and no model/network credential.

The pinned Rust 1.92.0 / Linux final gates passed:

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
cargo build --locked -p aicoder-cli
# Retain the default binary before the optional feature build.
cargo build --locked -p aicoder-cli --features package-downloads
python3 scripts/check_package_downloads.py --disabled-binary /path/to/default-aicoder
python3 scripts/check_package_graph.py
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-runtime -p aicoder-eval --features server,host-extensions,mcp-fixtures --bins --examples
python3 scripts/check_host_extensions.py --packages
python3 scripts/check_package_themes.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
```

**19 gates, 642 default workspace tests, 733 all-feature tests and 226 isolated core tests**
passed. Existing single-source, Git quarantine, local import/dependency/trust, frontend,
coding archive and SWE contracts stay green. CI includes the graph driver; local results
do not claim hosted CI execution. No default feature/dependency or persisted format changed.

The catalogue admits at most 32 entries / 256 KiB. The closure defaults to at most 32
packages and 256 MiB each of archive/decoded bytes, under one acquisition deadline; npm
metadata has its separate bounded per-request/count overhead. Failed validation/publication
can leave quota-admitted inert objects, but cannot publish a partial selected/trusted graph.
Post-visibility durability errors retain the existing inspect-before-retry contract. The
native resource format remains distinct from npm JavaScript/TypeScript ABI resolution;
SSH transport and broader distribution formats are not claimed by these HTTPS/npm checks.

The [Pi matrix](pi-parity-audit.md) now records native remote dependency-source acquisition
as implemented. Typed tool media, extra Code Mode helpers, hook breadth and the final
isolated real-service comparison remain open. This is not full Pi/release acceptance.

The [default footprint record](baselines/s7-package-graph-report.json) retains all three
final-source samples, with shared source SHA-256
`b64d1ed4bb0c3a0ba30d4ffbf7a5b469a03f34657086bc10cea1f71fa30b969b`.
Revision is the parent because measurement precedes commit; default resolved package
inventory remains 272. No lockfile/dependency/default-feature change was made.

| Default metric | Observation | Change from single-source acquisition |
| --- | --- | --- |
| Release CLI | 17.490 MiB | +0.213% |
| Release TUI | 17.567 MiB | +0.153% |
| Release SWE-bench | 17.290 MiB | +0.118% |
| 125-case smoke wall time, median | 387.11 ms | -1.100% |
| Smoke process peak RSS, median | 8,192 KiB | +4.918% |
| Idle offline runtime RSS, median | 4,288 KiB | +1.804% |

Wall samples are **387.11 / 811.17 / 381.25 ms**, all retained. The unisolated outlier is
not attributed without evidence. These cached default observations do not measure active
graph staging, decoder/network/quarantine or rendered frontend memory and guarantee no
latency. No new real model/Pi comparison was performed.
