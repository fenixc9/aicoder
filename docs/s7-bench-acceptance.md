# S7 coding benchmark archive acceptance

The [coding benchmark entry](coding-benchmarks.md) now runs a small pinned Rust/JSON suite
through EvalRunner/core/runtime, grades independently after the agent ends and saves a
verifiable archive. This completes the S7 benchmark metadata/metrics/artifact implementation
items on Linux. It does not complete S7's real-provider/Pi comparison or release audit.

## Verification

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-eval --features server --bins --examples
python3 scripts/check_benchmark.py
python3 scripts/check_extension_ui.py
python3 scripts/check_extension_ui_browser.py
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_images.py
python3 scripts/check_web_browser.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_process_protocol.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**608 default workspace tests, 672 all-feature tests and 226 isolated core tests** pass.
Twenty-three focused benchmark tests and two additional trace tests establish:

- Three actual Rust repairs and a JSON edit use the existing prepared tools and independent
  behavior graders. A different correct Rust implementation passes; unchanged initial bugs
  fail. Model request capture confirms hidden test source and grading metadata are absent.
- Execution-derived request/options, model, tools/capabilities, dispatcher/run budgets,
  context policy, native catalog and resource provenance are pinned. Changing actual limits
  or changing a tool's host capability with an identical schema rejects comparison.
- Every selected failure remains in the denominator: provider error, limits, setup failure,
  ungraded runs, cancellation and incomplete/failed archives. Later cases continue after a
  factory failure; unknown aggregate consumption remains unknown. No implicit resume occurs.
- Native archive round-trip failure and injected artifact conflicts do not repeat execution.
  Known metrics and successfully written forensic files remain available with a lower verdict.
  Existing output is rejected before another agent call. Publication never replaces a manifest.
- SHA-256, byte counts, scoped references, outcome/stop/status, terminal event usage/counters,
  grader verdicts, trace completeness, diff/patch and derived totals are checked. Altering
  report metrics or grader outcomes and updating file digests still fails internal validation.
- Identical bytes through a leaf/parent symlink are refused. Linux descriptor anchors preserve
  the chosen archive scope. Complete UTF-8 snapshot patches apply with actual git apply.
- Trace count/byte overflow retains a contiguous prefix and reports the first missing event.
  Array framing fits the byte budget; oversized trace publication preserves the previous file
  and discards temporary staging. A partial stream cannot become a complete replay.
- Snapshot/file/entry/depth limits, link targets and executable bits are represented explicitly.
  A post-run observation failure retains completed progress, usage and the actual event trace;
  unavailable workspace data cannot become an unchanged-workspace success.
- Grader command arguments remain literal; timeout terminates the owned process group.
  Credential-reference exclusions, runtime capture bounds and independent dropped-future
  cleanup are reused. The CLI refuses provider config without explicit --live.

The CLI regression executes two full offline runs and verifies equal-input comparison/replay.
It also records four failures each for noop, provider-error and limit variants. Six archives
verify; an explicit loopback HTTP provider executes one JSON task with three fake model requests.
The default offline runs make zero model network requests. No real credentials or remote model
requests are used. These synthetic scores prove contracts, not LLM coding success.

Existing terminal/browser regressions, image flows and SDK/CLI/RPC/Web normalized parity remain
green; process/event/RPC/export fixtures retain 12/337/44/6 entries. CI runs the coding regression
and preserves its fake-provider archives with the official
[upload-artifact action](https://github.com/actions/upload-artifact#usage). CI itself is configured
here; hosted CI execution/upload is not claimed by local verification.

## Footprint and evidence

The [default footprint report](baselines/s7-bench-report.json) records final-source samples and
limitations. The [coding smoke evidence](baselines/s7-bench-smoke.json) records verified archive
summaries, source/binary/toolchain pins and explicit scripted/loopback labels, without committing
raw sessions or generated run directories. Raw traces/diffs remain explicit runtime output.

The new dependencies in aicoder-eval (ring, libc, shlex, similar) already exist in the locked
workspace. No package version or resolved inventory increase is required. Production core
gets only immutable configuration/catalog observations and serde for its existing settings;
no HTTP/UI/benchmark fixture dependency enters core. The new coding binary and grading logic
remain in eval. Embedded JSON fixtures now participate in the baseline source fingerprint.

Three final-source samples share baseline-collector Rust/manifests/assets SHA-256
`61305abed05accc81420a35deed6a2695b065f35b6592cf114ac7219b5c3aa09`.
The benchmark's source stamp covers the same 288 files but sorts normalized relative path
strings, while the existing baseline collector sorts Path components. Its independently
defined source SHA is `7054839f25e9b8fb005e0dfa195220ab859b1d3c8c0e23441303b7c1550ab885`.
Both fingerprints and the actual executable hash are retained; no stale binary/source match
is inferred by comparing these differently ordered collectors.

| Default metric | Observation | Change from Code Mode |
| --- | --- | --- |
| Release CLI | 17.343 MiB | -0.001% |
| Release TUI | 17.310 MiB | -0.003% |
| Release SWE-bench | 15.723 MiB | +7.76% |
| 125-case smoke wall time, median | 396.10 ms | +2.83% |
| Smoke peak RSS, median | 7,936 KiB | -1.59% |
| Idle offline runtime RSS, median | 4,264 KiB | +0.85% |

The exact wall samples are 396.10 / 398.29 / 382.38 ms. Default help medians remain approximately
3.72–4.14 ms. The SWE-bench increase comes with shared evaluation capture/provenance and bounded
grader machinery; ordinary CLI/TUI binaries remain effectively unchanged. These observations
stay within the initial review thresholds; small differences are not optimization claims.
Builds reused existing targets, so no clean-compilation improvement is claimed.

The new standalone release coding binary is 16.450 MiB. Its four scripted contracts pass
using eight actual tool calls and 60 synthetic reported tokens, with 16 verified artifact
files. Case median/p95 are 120/143 ms for this observation, including grading; sampled
benchmark-process RSS reaches 14,152 KiB and cost is unknown. This is a separate workload
from the default 125-case smoke, excludes compiler/tool child RSS and archive staging from
its per-case memory sampler, and proves no real-model coding score or Pi performance parity.

## Remaining S7 work

Real-provider smoke and task scores have not been collected in this change. The --live entry
is ready for an explicitly selected provider/model and fixed budgets; loopback tests do not
substitute for a real service run. The source is dirty during pre-commit measurement and the
recorded revision is the parent, so each observation retains its actual source fingerprint.

The full feature-matrix audit, a pinned Pi run under comparable tasks/model/budgets, official
SWE-bench generation/grading review and release validation remain open. The legacy SWE-bench
checkout/patch preparation/resume path retains its previous host-side behavior and needs its
own final audit; this archive implementation does not certify it. Native code/Bash retain OS
permissions. The archive's hashes validate consistency, not publisher authenticity or protection
against deliberate same-user benchmark cheating. Memory samples exclude child processes and
later archive staging; cached usage/unknown rates cannot become exact billed cost.
