# S4 semantic summary acceptance

Core now supports bounded automatic and manual semantic summaries, durable branch
checkpoints, retries, deterministic fallback and summary accounting. CLI `compact-session`
and TUI `/compact` share the same core operation. This completes the second and third
S4 roadmap items; S4 and full Pi parity remain open.

The [summary contract](semantic-summary.md) specifies source/output limits, exact retained
anchors, uncertainty, cancellation, accounting, compatibility and runtime configuration.
Legacy Compaction records retain their old metadata-only behavior. New summary data is
stored as reserved versioned extensions; JSONL stays v3 and events stay v1.

## Verification

Passed with pinned Rust 1.92.0 on Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-runtime --example offline_summary
cargo run --locked -p aicoder-runtime --example offline_smoke
python3 scripts/check_event_schema.py
```

The existing context, approvals, edits, file-tools, shell, resources and runtime examples,
plus the core provider, embedded-agent, controlled-agent, event-stream and overflow examples,
also execute successfully. The new JSONL example is part of CI. It uses a scripted provider
and checks raw-history preservation, branch isolation, durable usage and resume without
regeneration. Its sanitized [event-v1 trace](protocol/summary-context-v1-trace.jsonl)
shows resumed checkpoint provenance; automatic summary/fallback traces are also validated
in the core tests.

**384 workspace tests**, including **152 isolated core tests**, pass. All prior 364 tests
remain, with 18 new core summary tests, one runtime configuration test and one TUI reducer
test. These exercise bounded retries/chunks, all-attempt accounting, free budget preflight,
unknown usage estimates, provider cancellation and duration expiry, complete tool units,
opaque-state omission, branch restoration, checkpoint rejection, persistence failure,
failed-source caching/manual retry, uncertainty-preserving fallback, complete event traces,
and merging a prior checkpoint as new history grows. The TUI test verifies that manual
compaction adds no user message and restores known durable usage. Configuration checks
cover strategy/output precedence, invalid bounds and requiring an explicit context window.
The schema checker validates **215 frames**, including the 18-frame sanitized resume trace.

## Lightweight measurements

[All three samples](baselines/s4-summary-report.json) use the unchanged S0 collector,
three warm-help samples and 25 iterations of five existing smoke cases. The exact final
Rust/Cargo source digest is `39f15f066878df7549af149062d036f489d00ea980c92b63016cce29620e081f`.
Reports refer to parent commit `8d1a2b5` with a dirty worktree; that digest identifies the
actual feature build. No clean-build timing claim is made. The reference is
[s4-projection-report.json](baselines/s4-projection-report.json), using the same toolchain,
Linux host and measurement procedure. Resolved packages remain **249**.

| Release executable | Size | Change from projection increment |
| --- | ---: | ---: |
| CLI | 13.05 MiB | +4.65% |
| TUI | 13.43 MiB | +4.46% |
| SWE-bench | 12.99 MiB | +1.91% |

The 125-case smoke samples take **391.0, 386.3 and 1093.4 ms**. All samples are retained;
the third is a large elapsed-time outlier. Median is 391.0 ms versus 379.2 ms (+3.11%).
Peak RSS samples are 7040, 7168 and 7168 KiB (median 7.00 MiB versus 6.625 MiB, +5.66%).
Median idle runtime RSS is 4088 versus 4052 KiB (+0.89%). Current review budgets are not
exceeded. The extra compiled API/validation/projection and frontend paths explain additional
binary code; three short observations cannot establish statistical performance equivalence.

The separate semantic-summary JSONL fixture takes **27.20, 27.98 and 27.71 ms** including
process, temp-file and synchronization overhead; peak RSS is 5632, 5632 and 5504 KiB.
Each fixture merges three scripted summary calls, reduces estimated context from 2230 to
131 tokens, records 30 summary tokens, then resumes once for 10 additional coding tokens
without regenerating the summary. These values describe this deterministic fixture,
not production compression ratios or real provider cost. Summary source is checked against 2 MiB before sanitization; ordinary context is borrowed
during summary selection where no recovery normalization is needed. Hash/size checks stream,
chunk selection avoids quadratic growing-payload serialization, and deterministic pruning
now computes removal costs before rebuilding once.

Existing collector limitations remain: caches are not cleared, help includes process/time
wrapper overhead, idle RSS is not rendered-TUI memory, and no allocator/message-copy
instrumentation or network/real-model quality benchmark is performed.

## Scope and limitations

No new crate/dependency is introduced. Default automatic strategy remains pruning; semantic
mode and manual commands make real provider requests only when explicitly configured or
invoked. Provider errors never echo their response body into summary diagnostics. Dynamic
summaries remain User-role context data, and summary requests have no tools.

Success checkpoints survive restarts. Failed-source suppression is a bounded per-compactor
cache, so fresh processes may retry an unchanged failure. Manual retry is explicit. Durable
usage is known recorded usage, not a reconstruction of unobserved work after process loss.
Manual reports are separate from coding-run event streams; full CLI/RPC operation protocols
remain S5 work.

A summarized request is bounded by estimated token capacity. The snapshot and canonical
history still scale with loaded history, and the projector still walks the raw branch before
reducing its view. This increment does not claim a total-history memory bound or exact token
count. Tool-output artifacts, richer navigation/model switching and tokenizer policy remain
S4 work. Scripted constraints are preserved in contract tests; real model summary quality
and Pi task-success parity require later controlled evaluations.
