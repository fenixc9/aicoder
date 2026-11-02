# S7 isolated comparison acceptance

The [manual comparison](pi-comparison.md) now freezes application code and runs every
external agent and candidate grading process in fresh Linux namespaces. This closes the
observed shared-/tmp execution path for future cohorts. The original Spark smoke stays
unchanged, with its original exploratory limitations and all eight original outcomes.

`SourceStamp::load_observation` and the observe-source/run flags let an isolated benchmark
use an explicit pre-launch observation without mounting source or credentials. Three focused
Rust tests cover the executable/platform/compiler binding, bounded malformed/symlink inputs,
actual source-free CLI execution and rejection before output/model admission. Source
descriptors remain metadata; default source capture/parameters and archive schemas are
compatible. No core/runtime/provider dependency or execution-loop change was needed.

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
cargo build --locked -p aicoder-eval --bin aicoder-bench
python3 scripts/check_pi_comparison.py --output /tmp/fresh-isolation-acceptance
python3 scripts/check_pi_isolation.py
python3 scripts/compare_pi.py --verify /path/to/first-spark-smoke
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
```

All 15 final gates passed: **633 default workspace, 700 all-feature and 226 isolated core
tests**. Five offline cohorts each retained four tasks per agent: correct, noop, provider
error, unknown usage and tool overflow. Correct contracts passed 4/4 on both; every intended
failure remained 0/4. This uses loopback fixtures, not a real model. The existing coding/SWE
archive contracts and read-only verification of the first Spark archive also passed.

The separate namespace test proves host private paths and host PIDs are not visible, system
and frozen code mounts reject writes, owned case writes work, /tmp does not persist between
runs, source edits do not affect frozen code, and directories/internal links are pinned.
Escaping links and .env files reject before execution. An actual fork detaches its session,
signals readiness, and stops with the namespace owner after timeout. This proves stopped
execution; adopted zombie reaping may still be pending in host init.

Bubblewrap 0.6.1 was actually used. A trivial Rust compile/link/run is checked before any
model request; failure has no host-execution fallback. Host networking remains shared and
temporary filesystems use the kernel default size. These are filesystem/process boundaries,
not a complete network, kernel or resource sandbox. Source observations and unsigned hashes
do not authenticate a publisher, model result or arbitrary historical execution.

The [footprint/contract record](baselines/s7-pi-isolation-report.json) retains all three
final-source default samples. Source SHA-256 is
`4ceb82d400000d07041760e8f56b441e32aab5d670722ff8b7d3200779f0c18e`;
the revision is the parent because collection precedes commit. Resolved packages remain
272 with no Rust dependency or lockfile change. Python, Node, Pi and Bubblewrap remain
optional development prerequisites. The recorded frozen debug-input generation had
16,443 entries and 402,738,875 file/link bytes, including debug symbols; it is temporary
development storage, not a default agent dependency or resident-memory measurement.

| Default metric | Observation | Change from stock-host activation |
| --- | --- | --- |
| Release CLI | 17.410 MiB | 0% |
| Release TUI | 17.481 MiB | 0% |
| Release SWE-bench | 17.256 MiB | +0.021% |
| 125-case smoke wall time, median | 388.88 ms | -0.463% |
| Smoke process peak RSS, median | 8,064 KiB | 0% |
| Idle offline runtime RSS, median | 4,200 KiB | -1.593% |

All smoke wall samples are retained: 385.47 / 388.88 / 388.90 ms. Cached unisolated host
measurements imply no latency guarantee. The release benchmark entry is 16.810 MiB;
its historical comparison includes intervening SWE/host features, not just these flags.
Current comparison launcher RSS is explicitly **not agent RSS** and cannot compare agent
memory. The default footprint above uses the existing offline runtime workload.

No additional real-service calls were made during this work. A larger isolated paired
Spark sample, full feature matrix and release acceptance remain open.
