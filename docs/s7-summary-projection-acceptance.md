# S7 summary projection acceptance

This increment closes the newest-edited-tail refusal in built-in manual/automatic semantic
summaries. Stable IDs map the actual model projection to raw history; synthetic repairs/summary
messages do not acquire canonical IDs. New checkpoints count all real message contributors while
v1 keeps its original rules. See [contract](summary-projection.md).

Nine added core tests cover latest edits without new raw input, custom/native boundaries and
counts, reordered/split result records, omitted identical-text tails, automatic retention with
fresh input, paid cancellation, stale same-length views/history, unknown IDs, v1 mixed-role counting
and omitted recovery uncertainty. The existing summary suite also covers retries, budgets,
checkpoint failure, replay, growing branches, protected provider state and deadlines. Four runtime
tests cover cold manual/automatic summaries, mixed roles, global counts, raw-file prefix identity,
real reopened payloads and read-only live projections with stale/foreign-turn rejection.

The full/window `offline_context_edits --compact` variants each make one scripted summary request
and one main request, preserve the original target and ten canonical messages, and resume through
the existing Agent loop. They use no model credentials or external network. CI runs both variants
and the codec-free runtime suite. Stock CLI/TUI manual/automatic summary paths share these APIs;
this increment introduces no new frontend command or owner loop.

Linux / Rust 1.92.0 passed **14 final local checks**, including all required checks,
with **895 default workspace tests, 1032 all-feature tests and
410 isolated core tests**. Default/all-feature Clippy and strict rustdoc passed,
as did the codec-free runtime suites, offline smoke and four executable variants. Commands/exits,
source identity and all measurements are retained in the [report](baselines/s7-summary-projection-report.json).
These are local results; CI has not been observed here. No external model request was made.

All 272 resolved packages remain unchanged, as do manifests, lockfile and default features.
Deterministic pruning skips the extra journal projection; regular window loads borrow origins and allocate owned IDs only for explicit lineage requests. The three cached release
samples pin source SHA-256 `8d3b1dd6b1974e9d856ff8d7219ae66a0fdd91aeb839b9ecabd2d79f8c912ad0` and the parent revision; executable hashes are retained.

| Workspace-default observation | Median / size | Change from context-edit baseline |
| --- | --- | --- |
| Release CLI | 21.135 MiB | +0.243% |
| Release TUI | 21.306 MiB | +0.245% |
| Release SWE-bench | 20.267 MiB | +0.145% |
| 125-case smoke wall | 1046.54 ms | +161.958% |
| Smoke peak RSS | 9344.00 KiB | +0.000% |
| Idle offline runtime RSS | 4480.00 KiB | +2.097% |

The wall reference uses the previous context-edit [workspace follow-up](baselines/s7-context-edits-workspace-followup.json),
with median 399.51 ms; its initial slower samples and matched investigation remain unchanged.
Wall samples **1034.00 / 1046.54 / 1069.43 ms** are retained without filtering. Shared-host warm-cache timing
changes are observations, not causal performance claims or exclusion of smaller regressions.
Default smoke/idle do not execute active summaries/edits or rendered frontends; they do not bound edit
latency, archive/projection allocation peaks or isolated agent memory. Offline measurements do
not establish real-model quality, prices or billed usage. No standalone-eval isolation claim is
added by this increment. Boundary drafts, compaction interception/replacement, tree summaries
and final isolated Spark/Pi/SWE quality/release acceptance remain open.

The initial **+161.958%** wall observation triggered [independent matched builds](baselines/s7-summary-projection-matched-report.json).
Parent/candidate use separate owned checkouts and fresh targets with `images` selected. Compiler
artifacts bind the three workspace libraries and example to their explicit checkout manifests;
source hashes were checked before/after building and sampling. Ten alternating AB/BA pairs cover
**2,500 fixture case executions**, retaining all twenty wall samples and per-case count/median/max.
Reference/candidate medians are **1036.70 / 1088.88 ms** (+5.033%; paired median +3.200%). Pair changes
range from **-28.570% to +36.883%**. Both owned checkouts and target directories were removed.
These runtime-only builds do not assert identity of the complete workspace feature graph.

[Later workspace observations](baselines/s7-summary-projection-workspace-followup.json) retain
**400.46 / 374.38 / 387.11 ms**, including all 375 per-case timing observations. Their executable
hash equals the hash recorded for each of the three initial samples. No main workspace rebuild
ran in between. A large absolute shift thus occurred without changing executable bytes; host and
storage causes have not been isolated. The matched candidate median remains higher and smaller
regressions are unresolved. No causal speedup or no-regression claim follows from these numbers;
final performance/quality acceptance remains open.
