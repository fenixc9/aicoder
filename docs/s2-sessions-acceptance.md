# S2 session storage and recovery acceptance

Historical increment record. [Final S2 acceptance](s2-events-acceptance.md) supersedes
the remaining-stage status below; these measurements and test counts remain archived.

This completes the session tree, storage/migration and interrupted-interaction
items of S2. RunHandle steering/follow-up, versioned event subscriptions and
backpressure remain open; this is not full S2 or Pi parity acceptance.

## Delivered behavior

- Shared object-safe repository contract: atomic batches, selected ancestry,
  revision conflicts, metadata records and owned run leases. Public domain
  reconstruction/validation/folding APIs support external storage implementations.
- JSONL v3 checksummed transactions, recoverable torn tails, stale-edit detection,
  independent metadata/run locks, private Unix files and backed-up v1/v2 migration.
- Assistant tool-call checkpoint before dispatch, each completed result saved,
  terminal messages/stop/partial/accounting saved before terminal events.
- Safe continuation constructs missing error results ephemerally, retains original
  call order, distinguishes interrupted/crashed turns and never resumes operations
  automatically. Uncertain external side effects require workspace inspection.
- CLI session-tree/JSON inspection, checkout and migration commands; TUI reopens
  the selected branch and labels interrupted/partial output.

See [the contract and limitations](sessions.md) and [API migration](migrations.md).
Compaction and extension entries are storage facilities; automatic semantic summaries
and extension loading are still future roadmap items.

## Verification

Validated on Linux x86_64 with pinned Rust 1.92.0, without model credentials or
model-service network calls:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo run --locked -p aicoder-core --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
```

All passed: **202 workspace tests**, plus the smoke example's five cases.
The same store contract tests run against memory and JSONL. Coverage includes
preserved branches, invalid-parent atomicity, stale edits, model/extension/compaction
metadata, run ownership, out-of-order/missing tool results, matching turn markers,
legacy exact backups, transaction corruption/torn batches, and a subprocess that
exits without releasing its lease through Rust destructors. Executor tests cover
provider failure after tools, task destruction/recovery, cancellation with only a
later parallel result completed, journal failure retaining tool progress, and
persistence visible from terminal callbacks. CLI management and TUI reload have
frontend regressions.

## Lightweight measurements

No new runtime dependency was added. TUI gains `tempfile` as a test-only dependency,
already in the workspace graph; the lockfile still resolves 234 packages.
The [three raw reports](baselines/s2-sessions-report.json) retain the measured source
digest and parent revision; `worktree_dirty` is true because measurements preceded
this feature commit. All three reports use the same source digest.

| Release executable | Size | Change from S1 provider baseline |
| --- | ---: | ---: |
| CLI | 9.75 MiB | +4.54% |
| TUI | 10.09 MiB | +3.13% |
| SWE-bench | 9.97 MiB | +0.23% |

Each runtime sample executes 25 iterations / 125 offline smoke cases. Consecutive
samples took 318.9, 319.2 and 326.3 ms (median 319.2 ms), versus S1's historical
313.1 ms sample. Peak RSS was 5248, 5120 and 4992 KiB (median 5.00 MiB), versus
4.75 MiB in that S1 sample. These changes remain within the initial review budgets;
single historical samples do not establish a statistically significant regression
or improvement. Builds used an existing target directory, so no clean-compilation
speed comparison is claimed. The script also samples help startup and idle runtime
RSS; those are not interactive TUI startup/memory measurements.

Extra storage cost is explicit: tool calls/results require durable checkpoints;
normal turns batch their initial and terminal entries. The journal indexes stored
entries instead of retaining another copy of each checkpoint payload. Current file
conflict detection reads the whole file per commit, and all branches remain retained;
large-history I/O and archival remain limits to address with measured workloads.
