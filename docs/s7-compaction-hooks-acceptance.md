# S7 compaction interception acceptance

Automatic and manual compaction now share native before decisions, scoped model work, owned
replacement preparation and post-effect observation. Callbacks veto, replace with a native
reduction, customize default guidance or resume default generation after an earlier replacement.
A committed checkpoint remains applied when observation later fails. See [contract](compaction-hooks.md).

Ten added core summary tests cover automatic/manual veto and native replacement, separate helper
usage, default guidance append/replacement without direct instruction persistence, invalid targets,
observer failure/deadline, interrupted idle receipt settlement, instruction-aware failed-source
caching and ordered restoration of default generation. Three runtime tests cover manual/automatic
cold-window targets, raw-prefix identity, veto, rejected unadmitted tails and retained paid work.
An evaluation-journal test validates trace counts and primary/auxiliary subtotals. A TUI regression
checks acknowledgement/presentation and conservative cost display after incomplete post work.
Existing summary, control, boundary, cancellation and provider tests remain part of the workspace.

Twelve executable variants use real JSONL/full/window sessions with scripted providers. Native
overrides make one helper request (usage 3), plus one main request (usage 7) in automatic mode;
manual mode creates no fictitious turn. Veto variants make no model request and commit no reduction.
Guidance variants retain default structured generation and omit guidance text from receipts/history.
Observer failure retains the acknowledged checkpoint and reports delivery diagnostics separately.
These are offline contracts, not coding-quality scores or real billing results. CI includes all
variants and the codec-free runtime suite.

Linux / Rust 1.92.0 passed **41 final local checks: 929 default workspace tests,
1066 all-feature tests and 433 isolated core tests**. Required fmt, Clippy,
core/workspace tests, offline smoke and strict rustdoc passed, as did all-feature Clippy/docs,
22 codec-free runtime tests, twelve new variants, earlier boundary/edit/summary variants, benchmark CLI/archive and
loopback frontend/schema regression. The evaluation-journal trace validator and TUI acknowledgement
presentation regression pass.
Exact commands/exits and all measurements are in the [report](baselines/s7-compaction-hooks-report.json).
These are local checks, not observed hosted CI results. No external model request was made.

All 272 resolved packages, manifests, lockfile and default features remain unchanged. No compaction callback
phase or extra hook snapshot is created when no compaction hooks are registered. Samples pin source
SHA-256 `29033ce4af2cda53d6d2846ffffdb519071693f69e029edd9a345f2f7fcb2680` and the parent revision; executable hashes are retained.

| Default workspace observation | Median / size | Change from boundary-draft baseline |
| --- | --- | --- |
| Release CLI | 21.671 MiB | +1.137% |
| Release TUI | 21.833 MiB | +1.008% |
| Release SWE-bench | 20.447 MiB | +0.561% |
| 125-case smoke wall | 401.29 ms | +5.359% |
| Smoke peak RSS | 9728.00 KiB | +1.333% |
| Idle offline runtime RSS | 4420.00 KiB | -1.603% |

Wall samples **401.29 / 396.98 / 1180.10 ms** remain unfiltered. The reference uses the preceding report's same-collector
post-build samples. The earlier summary-lineage [workspace follow-up](baselines/s7-summary-projection-workspace-followup.json)
and [independent matched observations](baselines/s7-summary-projection-matched-report.json) retain the
earlier timing variation; no data is replaced or treated as a causal speedup/no-regression proof.
Default smoke/idle do not execute active compaction hooks/model work or rendered frontends, and do
not bound their allocation peaks, latency or isolated agent memory. No standalone-eval isolation
claim or real-model coding/billing claim is added. Tree-summary customization and final isolated Spark/Pi/SWE quality/release acceptance remain open.
