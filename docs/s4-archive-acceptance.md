# S4 bounded archive loading acceptance

Core/Runtime now support explicit materialized run windows and canonical archive pages.
Runtime streams validation/fingerprint verification with record, index, snapshot, page and
scan budgets. Agent continuation, model checks, summaries, navigation and TUI loading can
use bounded real tails while canonical JSONL and inactive branches remain intact. No new
dependency is added. See [archive contracts and limits](archive-windows.md).

## Verification

Passed with pinned Rust 1.92.0 on Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo run --locked -p aicoder-runtime --example offline_archive
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
python3 scripts/check_event_schema.py
```

**447 default workspace tests**, **449 with optional features**, and **187 isolated core
tests** pass. All prior 440/442 tests remain. Seven real-filesystem additions prove:

- A snapshot exceeding its load budget can still continue through a bounded real tail;
  original archive bytes remain a prefix, new records persist, and cold payload/index
  descriptors never enter the model request or canonical progress.
- Canonical pages match original entries, tree/search read real cold text, stale cursors
  fail, and global names/labels plus cold-branch checkout/fork retain their semantics.
- Old windows conflict, same-length corruption fails, and even identical copied archives
  do not allow a window to write through another repository.
- Record/index/scan/page budgets fail without file mutation; newest oversized tool units
  are not split, and a torn final transaction is repaired as one batch before append.
- Window leases, foreign-workspace rejection and non-restoration of ownership hold.
- A durable summary retains the fixed KEEP_CONSTRAINT task condition after window loading;
  continuation and a later window-backed manual checkpoint preserve global source counts
  and explicitly record archived raw input.

All **18 offline example executions** pass, including the new archive reader. Its complete
22-frame trace validates inside the example. Existing context, policy, summary, artifact,
navigation, approvals, edits, shell, resource, runtime and event/overflow paths remain
covered. CI executes the new example. Event-v1 source schema accepts the optional archived
count, and old data defaults to zero.

## S4 requirement audit

| Requirement | Evidence |
| --- | --- |
| Raw history separate from provider views and branch provenance | Projection contracts/tests plus real archive pages/tails and unchanged canonical prefix |
| Automatic/manual semantic summary and traceability | Existing summary suite plus fixed-constraint checkpoint/window test |
| Retries/fallback, usage and no repeated successful compaction | Summary/policy suites and restored checkpoint continuation |
| Bounded tool output/full references and complete units | Artifact suite plus oversized-newest-unit archive rejection |
| Branches, labels, names, search, recovery and model selection | Navigation/model suites, backend pages and unified window mutation API |
| Optional text tokenizer, explicit request estimates/custom policy | Default/feature policy suites, no mandatory tokenizer dependency |
| History loading cannot grow RAM without a budget or exceed a configured model window | Bounded snapshot/record/index/tail/page/scan policy, streamed verification, production run views, counted context policy and independent larger-archive reader samples below |

S4's runtime continuation scope is now covered. Caller-retained Full Session/Memory-store
objects are still owned data structures, not a promise of unlimited bounded-RAM storage.
Budget exhaustion is an explicit result; arbitrary giant records or metadata counts are not
silently accepted. Large legacy logs must be migrated within declared budgets before window
loading. No claim is made about real-model task quality, exact remote billing or full Pi
parity; S5–S7 remain open.

## Lightweight and archive-reader measurements

[Three default samples and six archive-reader samples](baselines/s4-archive-report.json)
identify final Rust/Cargo source digest `d7cdd8a3632d15cbf58f3cae60c98da96dc6bb59e7644adc5f0a0004ae662f17`.
Reports refer to parent `c804f13` with a dirty worktree; this digest identifies the actual
build. Reference is [s4-policy-report.json](baselines/s4-policy-report.json) on the same
host/toolchain. Default dependency resolution remains **249 packages**.

| Default executable | Size | Change |
| --- | ---: | ---: |
| CLI | 14.08 MiB | +1.58% |
| TUI | 14.45 MiB | +1.55% |
| SWE-bench | 13.52 MiB | +0.03% |

The unchanged 125-case smoke samples take 389.5, 385.7 and 391.5 ms (median 389.5 versus
391.8 ms, -0.59%). Peak RSS samples are 7552, 7296 and 7424 KiB (median 7.25 versus
7.125 MiB, +1.75%). Median idle RSS is 4084 versus 4036 KiB (+1.19%). Current review
budgets are not exceeded. These are short process samples, not statistically established
improvements, allocator instrumentation or clean-build comparisons.

The archive generator runs in a separate process, outside reader measurements. Real v3
logs use 16 KiB user payloads in bounded transactions plus a final small message. The reader
uses a 1 MiB full-snapshot budget and 256 KiB materialized tail, continues through Agent,
prunes to an 896-token model input target and reads an original archive page.

| Generated payload messages | Archive before reading | Reader process samples | Peak RSS samples |
| --- | ---: | --- | --- |
| 512 | 8,508,602 bytes (8.11 MiB) | 121.64 / 116.55 / 115.53 ms | 9656 / 9504 / 9660 KiB |
| 4096 | 68,065,442 bytes (64.91 MiB) | 906.24 / 864.94 / 896.21 ms | 15372 / 15600 / 15600 KiB |

Reader RSS medians are **9.43 MiB and 15.23 MiB**. Every sample succeeds, loads only
16–20 real canonical messages and preserves raw source/provenance. Successive samples
append a new turn, so counts/file sizes grow slightly; all exact sizes/counts are retained
in the report. The larger archive does not become a full payload buffer in the reader.
Metadata still grows within its explicit budget, then errors rather than growing without
limit. Parsing/projection can use additional bounded temporary memory. IO/time increases
because validation/fingerprints scan bytes; this is not constant-time or constant-index
storage. A disk index or different backend can improve that later without changing core
run/page contracts.

These cases do not exercise the maximum allowed metadata count, arbitrary huge records,
multiple simultaneous archives, rendered-TUI memory or real-model responses. S4 acceptance
uses the declared budgeted production path, not an assertion that every SDK-owned Vec is
constant-space. All observations are retained without selecting favorable runs.
