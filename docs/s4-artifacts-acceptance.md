# S4 bounded output artifacts acceptance

Tool output now supports bounded previews backed by recoverable originals. Core exposes
storage/writer/page contracts and an in-memory implementation; runtime supplies private
workspace-scoped disk storage and raw shell streaming. CLI/TUI expose references, and the
model and credential-free CLI can read bounded pages through the same core tool. This
completes S4's tool-output/artifact item. Navigation/model switching and tokenizer policy
remain open; S4 and full Pi parity are not complete.

The [artifact contract](output-artifacts.md) defines scope, quotas, truncation, completeness,
publication, paging, failure and retention. JSONL remains v3 and event envelopes v1. There
are no new dependencies or background services, and core defaults install no storage/tools.

## Verification

Passed on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-runtime --example offline_artifacts
cargo run --locked -p aicoder-runtime --example offline_smoke
python3 scripts/check_event_schema.py
```

All 15 offline example executions pass: the new artifact example, existing runtime smoke,
summary, context, approvals, edits, file tools, shell, resources and runtime examples,
plus core provider, embedded-agent, controlled-agent, event-stream and overflow examples.
CI executes the new example. Its real shell/JSONL path captures **200,013 raw bytes**, sends
a preview to the model, recovers a hidden marker with `read_artifact`, and reopens the exact
bytes through a new store instance. It validates the complete **44-frame** event trace.
The schema checker validates **259 event frames**, including this example.
The [sanitized fixture](protocol/output-artifacts-v1-trace.jsonl) retains that reference
provenance and paging behavior without a real storage path or credentials.

**404 workspace tests**, including **161 isolated core tests**, pass. All prior 384 tests
remain, with these 20 additions:

- Nine core cases cover byte-exact binary paging/immutability, pending reservations and
  drop cleanup, generic success/failure originals plus live references, quota/oversized
  fallback without repeated operations, a display budget larger than the archive cap,
  complete escaped model pages/raw-offset progression, stalled storage timeout/cleanup,
  and retaining references plus a complete tool pair across an output-budget stop and
  session restoration, applying the same archive path to failed tool preparation, and
  preserving eight existing references without creating an unreachable ninth archive.
- Nine runtime cases cover reopening files, workspace mismatch, traversal/symlink rejection,
  private permissions, live/stale reservation cleanup, independent concurrent writers,
  hidden shell middle/binary-byte recovery, small results creating no files, partial
  archives at the archive cap, nonzero shell results and exhausted quota without reruns,
  scan-limit incompleteness, cancellation discarding active staging, and corrupt footer
  length rejection before unbounded allocation.
- One CLI integration case reads the expected raw offset without credentials or network,
  rejects a different workspace, and verifies `--no-session` clears persistent storage.
  One TUI reducer case verifies reference IDs and completeness remain visible beside a
  truncated tool preview.

## Lightweight measurements

[Three complete samples](baselines/s4-artifacts-report.json) use the unchanged collector,
three warm-help samples and 25 iterations of five existing offline smoke cases. The final
Rust/Cargo source digest is `cc58038364fd9e37f1d010de8142496b276b77deba6ec2ddbce850fe18bdc8f0`.
Reports identify parent commit `0958a73` with a dirty worktree; that digest identifies the
actual build. The comparison is [s4-summary-report.json](baselines/s4-summary-report.json)
on the same host/toolchain. Resolved packages remain **249**.

| Release executable | Size | Change from summary increment |
| --- | ---: | ---: |
| CLI | 13.47 MiB | +3.17% |
| TUI | 13.83 MiB | +3.01% |
| SWE-bench | 13.36 MiB | +2.91% |

The 125-case smoke samples take **383.1, 379.5 and 377.7 ms** (median 379.5 ms versus
391.0 ms, -2.94%). Peak RSS samples are 7168, 7040 and 7296 KiB (median 7.00 MiB,
unchanged). Median idle runtime RSS is 4108 versus 4088 KiB (+0.49%). Current review
budgets are not exceeded. No clean compilation timing claim or statistically significant
performance improvement is inferred from these short measurements. The extra compiled
storage/paging/serialization and frontend paths account for added binary code; no new
crate or dependency is introduced.

The separate artifact fixture takes **30.48, 25.33 and 25.11 ms**, including process,
temp-file, shell and synchronization overhead. Peak RSS samples are 8064, 7808 and
7936 KiB (median 7.75 MiB). Each run archives/reopens 200,013 bytes, recovers the hidden
middle with a model tool call, and validates 44 events across three scripted coding rounds.
This is an offline contract fixture, not a real-model latency or success-rate benchmark.
The baseline smoke deliberately retains its existing five cases; it does not exercise
worst-case archive size, multi-workspace accumulation, large custom JSON producers or
archive-disk throughput. All samples are retained without selecting favorable runs.

## Scope and limits

Known complete originals are recoverable within the configured archive policy; unlimited
outputs are not promised. Archive-cap/scan/time/capture stops are explicitly partial, and
storage failure yields bounded preview plus a diagnostic rather than tool re-execution.
ReadOnly model pages use explicitly lossy text; raw SDK pages retain binary data exactly.
A trace/reference alone does not export the original bytes, and hosts must retain/export
archives before discarding workspaces when future access is required.

The disk quota bounds admissions/reservations, with bounded footer overhead; it is not an
allocator/RSS guarantee. Completed artifacts are never silently evicted. Retention and
removal are explicit host operations. Process loss or publication/journal failure may
leave a final unreferenced file charged to the same quota; stale staging has an explicit
OS-lock-aware cleanup method. Scope/length validation is not cryptographic integrity
against the filesystem owner. Approved shell commands still have user permissions.

Source/context token estimates remain heuristic. Canonical snapshots and branch projection
still scale with loaded history; this increment does not implement history paging, richer
session navigation, an exact tokenizer, real-model quality evaluation or Pi success-rate
parity. The existing baseline collector measures short scripted cases and process/runtime
overhead, not worst-case archive throughput, rendered-TUI memory or model latency.
