# S4 token counting and context policy acceptance

Core now shares token counting between preflight, context decisions, counted compaction,
summary calls and budget accounting. Hosts can inject ContextPolicy and token counters;
runtime supplies optional local BPE encodings. This completes S4's tokenizer/custom-policy
item. Archive paging/loading budgets remain open, so S4 and full Pi parity are not complete.

The [count/policy contract](context-policy.md) separates exact text encoding, request framing
estimates, host calibration and provider-reported usage. Default builds retain heuristic
counting with no tokenizer runtime dependency.

## Verification

Passed with pinned Rust 1.92.0 on Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
cargo run --locked -p aicoder-runtime --example offline_context_policy
cargo run --locked -p aicoder-runtime --features tokenizer-tiktoken --example offline_context_policy -- cl100k_base
python3 scripts/check_event_schema.py
```

**440 default workspace tests**, **442 with all features**, and **187 isolated core tests**
pass. All prior 428 tests remain. Ten new core cases verify streamed legacy-compatible
heuristics plus response constraints; failed/zero/malformed/contradictory counter fallback;
input-driven output clamping and provenance; reported-usage precedence; policy rejection
with a valid closed trace and untouched history; stricter custom targets; known-window
protection; protected-prefix failure preserving consumed usage; counted summary chunks/
retries with a distinct summary model; and nonadditive candidate recounting with intact
tool-call/result pairs. Partial/contradictory usage floors are also checked.

Two default runtime cases cover settings precedence/feature availability and shared runtime
auto mode without model-name guessing. Feature-enabled cases add real BPE goldens for
English/Japanese and literal special-token spellings, text-vs-request precision, tool-schema
estimation, input caps and unsupported-content fallback. The adapter uses actual embedded
cl100k/o200k data from the locked library, not a fake byte/word tokenizer.

All **17 default offline example executions** and the optional BPE example pass. Existing
smoke, navigation, artifacts, summary, context, approvals, edits, file tools, shell, resources,
runtime, provider, embedded/controlled and event/overflow paths remain covered. CI executes
the new example and optional checks. Its custom headroom policy compacts to at most 200
input tokens, preserves canonical history and validates a complete **21-frame** trace.
The [default fixture](protocol/context-policy-v1-trace.jsonl) and
[BPE fixture](protocol/context-policy-bpe-v1-trace.jsonl) demonstrate the different methods
while retaining estimated full-request accuracy. The main SDK fixture was refreshed for
new optional provenance; legacy trace fixtures still read. The schema checker validates
**319 frames** across all published fixtures.

## Scope and limits

Text tokenization is exact only for its declared encoding. Neutral JSON/framing sums are
request estimates; vision, opaque state and remote hidden work are not certified by BPE.
Counter/policy implementations are trusted pure host code and own any exact full-request
calibration claim. Unknown usage remains estimated, and partial reported components cannot
be discarded in favor of an underestimated total/fallback.

Nonadditive pruning can remove more old units through geometric candidate selection,
but retains configured mandatory context and recounts actual candidates. Legacy compactors
keep their trigger estimate and must return a view that satisfies counted target/prefix
checks. Semantic compression remains bounded/retriable and can fail; counting does not
assert summary quality or model success.

Optional vocabularies add explicit build/startup/heap cost when enabled and initialized.
No network fetch occurs at runtime. Default active dependencies are unchanged; the lockfile
also records five optional package versions. Measurements below distinguish these paths.
This work does not page snapshots, bound total archive residence, or finish S4's memory
acceptance. It does not establish real-model quality, exact remote billing or Pi parity.

## Lightweight measurements

[All default and optional samples](baselines/s4-policy-report.json) identify exact final
Rust/Cargo source digest `1c50e8eefdce26ea723b58684a3ac8e315d642f884460298f7d96c9b7969c545`.
Default reports refer to parent commit `cd01f18` with a dirty worktree; this digest identifies
the actual build. The comparison is [s4-navigation-report.json](baselines/s4-navigation-report.json)
on the same host/toolchain. The default collector and its five-case smoke set are unchanged.

| Default release executable | Size | Change from navigation increment |
| --- | ---: | ---: |
| CLI | 13.86 MiB | +0.95% |
| TUI | 14.23 MiB | +0.78% |
| SWE-bench | 13.52 MiB | +1.21% |

The 125-case smoke samples take **383.7, 396.0 and 391.8 ms** (median 391.8 versus 389.1 ms,
+0.70%). Peak RSS samples are 7296, 7168 and 7424 KiB (median 7.125 MiB, unchanged).
Median idle RSS is 4036 versus 4128 KiB (-2.23%). Review budgets are not exceeded; these
small timing/idle changes are not claimed as improvements. No clean-build claim is made.
The stream estimator avoids another complete serialized-message buffer, but these samples
do not instrument allocator or whole-history clone counts.

Default Cargo metadata still resolves **249 packages**. The native workspace dependency tree
(including normal/build/dev selections used by this check) contains **198 versions**, exactly
the prior tree. Enabling BPE adds **five** versions: tiktoken-rs 0.12.0, fancy-regex 0.17.0,
bit-set 0.8.0, bit-vec 0.8.0 and rustc-hash 2.1.3. They are optional lockfile entries; core and
default runtime dependency selections do not include them. The report records the exact
selection difference, not just a lockfile total.

| Isolated policy example | Default heuristic | Optional cl100k BPE |
| --- | ---: | ---: |
| Release example size | 5.59 MiB | 11.19 MiB |
| Process samples | 2.72 / 2.48 / 2.37 ms | 61.84 / 61.56 / 59.06 ms |
| Peak RSS samples | 4736 / 4736 / 4736 KiB | 29704 / 29704 / 29668 KiB |

The optional sample was built separately after default collection; it does not replace
those CLI/TUI measurements. Each process initializes its own selected cl100k vocabulary,
so this is startup-plus-fixture cost, not steady-state encoding throughput. It does not
measure o200k initialization or multiple vocabularies retained in one process. Both modes
retain raw history, apply the same 200-token custom target and validate 21 events. Input
counts differ (100 heuristic versus 96 tokenized) and both remain **estimated**; ordinary
`hello world` text is three estimated heuristic tokens versus two exact cl100k tokens.
The fixture provider reports zero usage, independently of those input estimates.

All samples are retained. Existing collector limitations apply: no cache clearing, help
includes process/wrapper overhead, idle RSS is not a rendered TUI, and scripted cases are
not real-model latency/quality/billing or large-archive memory benchmarks. Optional cost is
explicit because bundled vocabularies and BPE initialization would materially change the
default footprint if enabled unconditionally.
