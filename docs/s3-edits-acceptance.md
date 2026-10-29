# S3 prepared edits, atomic publication and approval diffs acceptance

File edits/writes now prepare a bounded immutable operation before approval, present
its diff and commit that same operation with conflict checks. CLI/TUI and machine
approval events share the preview. This completes the S3 exact-edit/conflict/atomic-
write/diff item. Shell contracts and session grants remain open; S3 and Pi parity
are not complete.

## Evidence and behavior

- Core has host-neutral `PreparedTool` and optional `ToolPreview`, with a default
  preparation hook preserving custom tools' execution path. Plans prepare once and
  are consumed once after approval; denied/failed/oversized plans never execute.
- Runtime retains exact-match, ambiguity, overlap and all-or-nothing edit validation.
  Snapshots/results are capped at 4 MiB; optional read revisions guard earlier reads.
  New and replacement writes share the same preparation/commit implementation.
- A pinned parent, private exclusive temporary file, stable advisory lock, repeated
  byte/identity/mode checks, no-clobber creation, same-directory rename and file/directory
  synchronization replace the old loosely guarded writes. There is no background commit
  after cancellation. Read-only validation workers share immutable original bytes.
- CLI displays bounded diff review; TUI supports scroll and diff/argument switching
  with decision controls always visible. Events carry the same optional preview.
  Event schema remains v1, with old data still readable. Source-level struct/pattern
  additions are documented in [migration](migrations.md).

[The complete mutation contract](file-mutations.md) covers file/preview/receipt bounds,
revision strength, exact matching, link/path/mode handling, lock lifetime, cancellations,
durability errors, and the external-writer check-to-rename interval. Participating writers
coordinate through locks; arbitrary editors do not gain a kernel compare-and-swap.
This is not a filesystem sandbox or transaction with session persistence. Linux is the
verified platform. Existing image, shell and grant roadmap work is unchanged.

## Verification

All passed locally with pinned Rust 1.92.0 on Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p aicoder-core --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-runtime --example offline_edits
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo run --locked -p aicoder-runtime --example offline_file_tools
cargo run --locked -p aicoder-runtime --example offline_resources
cargo run --locked -p aicoder-runtime --example offline_runtime
cargo run --locked -p aicoder-core --example controlled_agent
cargo run --locked -p aicoder-core --example event_stream -- --overflow
python3 scripts/check_event_schema.py
```

**323 workspace tests** pass, including **112 isolated core tests**. All 308 prior test
names remain. Eleven new runtime cases cover reviewed bytes, external changes during
approval, external create collision, competing creates/replacements, read-revision
conflicts, invalid/denied operations, parent/alias replacement, dangling links/FIFOs,
mode preservation/conflicts, dropped approvals, staging deadlines, bounded/escaped
previews, oversized inputs and insufficient receipts. Core tests verify execute-once/
drop-on-deny, preparation timeout/preview overflow, and steer/cancel during preparation.
A Ratatui test verifies diff scrolling and argument switching with visible decision keys.

The offline example uses actual RunHandle approvals and the default coding registry with
a scripted model. It checks both successful publication and preservation of an external
edit after diff review, validates each complete event lifecycle, and can export its trace.
A normalized [64-frame approval fixture](protocol/approval-preview-v1-trace.jsonl) is now
checked alongside the existing 18-frame fixture: **82 schema-valid frames** in the default
checker/CI. The generated 64-frame trace also passed independently. Existing session,
resource, read/search, provider and SWE-bench fixture regressions remain green. Hosted
CI, real-model quality and official grading are not claimed as already run.

## Lightweight measurements and timing review

The graph grows from 248 to **249 packages**: similar 3.2.0, using std/text without
optional Unicode/bytes helpers. It provides a maintained unified diff implementation
with an algorithm deadline and bounded input/output rather than an ad hoc patch format.
Core gains no IO/provider/frontend dependency. No database, service or script runtime
is added. Coordination files are outside worktrees and bounded to 256 lock stripes.

[Three raw reports](baselines/s3-edits-report.json) retain the measured parent revision,
dirty-worktree flag and matching final Rust-source digest. Relative to the file-tool
increment:

| Release executable | Size | Change |
| --- | ---: | ---: |
| CLI | 12.36 MiB | +6.54% |
| TUI | 12.75 MiB | +6.13% |
| SWE-bench | 12.60 MiB | +6.27% |

Each sample runs 125 offline cases. Process times are 384.0, 376.3 and 375.6 ms
(median 376.3 ms, preceding median 290.7 ms, **+29.46%**). This triggers the existing
20% timing-review budget. Peak RSS is 6784, 6656 and 6912 KiB (median 6.625 MiB,
preceding 6.25 MiB, +6%). Binary and median-RSS changes remain below their 10%/20%
review thresholds. No budget has been silently reset.

All 75 per-case timing samples from each increment are retained in the report. Median
Edit case time changes from 579 to 3881 microseconds; Search changes 424 to 513,
ToolFailure 224 to 232, Cancel 139 to 139 and SessionResume 9278 to 9325. The measured
increase is concentrated in Edit. Its new path performs pre-approval preparation,
private staging, repeated snapshots/identity checks, lock coordination and explicit
file/parent synchronization. These observations do not separately isolate each source
of cost or establish a statistical confidence interval.

The default keeps these checks and synchronization because omitting them would weaken
the reviewed-write/crash contract. A fast-mode feature switch was considered and is
not introduced: it would change the meaning of a successful default coding write.
Hosts can register a different explicitly documented tool implementation through the
existing SDK registry when their storage model requires it. Read/model-only embedding
still uses core without this runtime backend. The timing increase is an explicit
tradeoff for this item, not a general performance improvement claim.

The complete two-scenario offline approval example takes 18.39, 14.26 and 7.81 ms
including process/temp-file overhead, with peak RSS 5632, 5760 and 5632 KiB. Raw outputs
are retained. Existing-target build/help/idle limitations apply; no clean-build, rendered-
TUI, hostile-filesystem latency, real-model or official benchmark comparison is inferred.
