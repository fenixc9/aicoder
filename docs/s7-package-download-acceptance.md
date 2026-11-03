# S7 remote root-package acquisition acceptance

The optional [Git HTTPS/npm acquisition contract](remote-packages.md) now has actual runtime
and CLI paths. It resolves/pins source versions, bounds transport/archive work and publishes
through the existing allowlisted PackageStore import. Installation never activates package
code. Native dependency pins must already be installed/trusted; automatic transitive source
selection remains open and this is not full Pi/release acceptance.

On Linux / pinned Rust 1.92.0, all final checks passed:

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
# Keep this default executable for the feature-rejection check.
cargo build --locked -p aicoder-cli --features package-downloads
python3 scripts/check_package_downloads.py --disabled-binary /path/to/default-aicoder
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-runtime -p aicoder-eval --features server,host-extensions,mcp-fixtures --bins --examples
python3 scripts/check_host_extensions.py --packages
python3 scripts/check_package_themes.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
```

**18 gates, 642 default workspace tests, 726 all-feature tests and 226 isolated core tests**
passed. Four extraction/version tests and six local-service integration tests prove:

- Actual Git HTTP backend fetch, full commit receipt and native resource import. A 20 MiB
  incompressible Git object is refused by a real 16 MiB private fetch filesystem, preserving
  the old registry. Separate namespace probes verified the size and zero effective
  capabilities after setup/setpriv. There is no unbounded host-clone fallback.
- npm scoped-name/tag resolution, concrete version/identity checks, strongest supported
  integrity selection and raw-byte verification. Default/no-feature CLI rejects before
  creating storage; enabled CLI gets the same native pin through Git and npm.
- Traversal/aliases, duplicate paths, links, devices, sparse metadata, inflation, entry/node
  depth budgets, dangling GNU/PAX paths, trailing data, gzip corruption and privilege bits.
  Every metadata header is admitted before interpretation; only regular files/directories
  are extracted, and only declared native payload reaches the final object.
- Wrong identity/digest, redirect and cross-origin tarball refusal without replacing a
  previously installed package; total multi-stage deadline and HTTP/Git cancellation.
- Explicit origin-bound bearer references through host environment. Values are absent from
  argv/public errors/provenance; deliberately credential-bearing persisted URLs are rejected.
  No model credential is required. Lifecycle fields/executable declarations do not run.

CI installs the optional Git quarantine prerequisites and runs the local-service CLI driver.
The local results do not claim hosted CI execution. The protocol tests use only loopback
fixtures, fake credentials and owned temporary repositories; no real model or public package
installation was performed. Existing executable/palette frontends and coding/SWE archives
remain green.

Private registry schema 2 stores remote provenance; local-only stores remain schema 1.
Current readers accept both, validate source fields and preserve public locks/pins/session
formats. Older binaries need a fresh local import of verified payloads before downgrade.
HTTP credential profiles are host-owned; source metadata cannot select a credential route.

The [default footprint record](baselines/s7-package-download-report.json) preserves three
final-source samples with shared source SHA-256
`9efdec903888cb39b860a90b24e2b421f34fba5c14ba9c694c7cc3d149b8abf5`.
The revision is the parent because measurement precedes commit. Two optional lock packages
were added (tar 0.4.46, filetime 0.2.29); both are absent from the default workspace graph.
The default metadata inventory remains 272 packages. Core has no new dependency; default
features are unchanged.

| Default metric | Observation | Change from package palettes |
| --- | --- | --- |
| Release CLI | 17.453 MiB | +0.042% |
| Release TUI | 17.540 MiB | -0.019% |
| Release SWE-bench | 17.270 MiB | +0.024% |
| 125-case smoke wall time, median | 391.41 ms | +0.210% |
| Smoke process peak RSS, median | 7,808 KiB | +1.667% |
| Idle offline runtime RSS, median | 4,212 KiB | -0.473% |

All wall samples are retained: **389.00 / 609.83 / 391.41 ms**. The unisolated outlier is
not discarded or attributed without evidence. Default cached smoke/idle observations are
not loaded transport/decoder/quarantine memory or latency guarantees. No Spark/Pi experiment
was rerun during this work.

The release CLI with package-downloads enabled is **17.871 MiB**, +2.398% from the default
CLI. This is executable size, not active transfer memory. The lockfile contains 287 packages
(285 previously), while default resolved metadata stays at 272; these scopes are distinct.
