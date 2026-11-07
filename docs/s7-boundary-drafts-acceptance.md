# S7 prepared boundary drafts acceptance

This increment adds sourced custom messages, persistent context edits and native compaction to
successful round/settlement boundaries. Pending record IDs stay fixed across callback previews,
fresh-message finalization and exact final publication. Canonical facts and pending/reduced model
views remain separate. See [contract](boundary-drafts.md).

Thirteen core tests cover chained pending-ID edits, retained/self-retaining reductions and future
records, whole-view admission, invalid/capability-denied atomic batches, fresh hidden custom
finalization, parent auxiliary limits, estimated unknown usage, cancellation/deadline cleanup,
discarded drafts, provider/callback/delivery failures, output clamping, primary/auxiliary separation
and recovery of completed receipts without terminal statistics. Five runtime tests cover cold
windows, zero-message resumption, archive prefixes/raw pages, future custom/assistant records,
real cold-target validation, larger-tail refusal, stale-source rejection and known receipt usage
from private metadata indexes. An evaluation-journal test validates the real event trace and
captures scoped work/drafts without a frontend or session-backed journal.

Eight executable variants use real owned file tools, approvals, JSONL and optional Code Mode over
full/window sessions. Allowed cases make two main requests and one scoped auxiliary request, retain
thirteen canonical messages and report scripted usage 14 primary + 3 auxiliary. Denied cases make
no auxiliary request, leave the file absent and retain twelve canonical messages with usage 14.
Tool counts are one ordinary call or two root/leaf Code Mode calls. These are contract fixtures,
not coding-quality scores or real service charges; no external model request occurs. CI includes
all variants and the codec-free runtime suite.

Linux / Rust 1.92.0 passed **26 final local checks: 914 default workspace tests,
1051 all-feature tests and 423 isolated core tests**. Required fmt, Clippy,
core/workspace tests, offline smoke and strict rustdoc passed, as did all-feature Clippy/docs,
19 codec-free runtime tests, eight new variants, four edit/summary variants, old boundary
examples and the benchmark CLI/archive regression. The evaluation-journal trace validator passes;
each committed native boundary batch emits one compaction completion and one matching terminal count.
Exact commands/exits and all measurements are in the [report](baselines/s7-boundary-drafts-report.json).
These are local checks, not observed hosted CI results. No external model request was made.

All 272 resolved packages, manifests, lockfile and default features remain unchanged. No callback
projection or model-work scope is created when no boundary hooks are registered. Samples pin source
SHA-256 `a55d990085ec30a584ab1fd591a8c08d1163c99712ec18001857962ee7410663` and the parent revision; executable hashes are retained.

| Default workspace observation | Median / size | Change from summary-lineage baseline |
| --- | --- | --- |
| Release CLI | 21.428 MiB | +1.383% |
| Release TUI | 21.615 MiB | +1.452% |
| Release SWE-bench | 20.333 MiB | +0.326% |
| 125-case smoke wall | 380.87 ms | -63.606% |
| Smoke peak RSS | 9600.00 KiB | +2.740% |
| Idle offline runtime RSS | 4492.00 KiB | +0.268% |

Wall samples **380.87 / 377.85 / 386.37 ms** remain unfiltered. The reference uses the preceding report's same-collector
post-build samples. Its later [workspace follow-up](baselines/s7-summary-projection-workspace-followup.json)
and [independent matched observations](baselines/s7-summary-projection-matched-report.json) retain the
previous timing variation; no data is replaced or treated as a causal speedup/no-regression proof.
Default smoke/idle do not execute active boundary drafts/model work or rendered frontends, and do
not bound their allocation peaks, latency or isolated agent memory. No standalone-eval isolation
claim or real-model coding/billing claim is added. Before-compaction interception/replacement,
tree summaries and final isolated Spark/Pi/SWE quality/release acceptance remain open.
