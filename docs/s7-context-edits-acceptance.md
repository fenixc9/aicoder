# S7 persistent idle context edits

This increment supplies durable branch-local model-view editing through the native session API.
It does not close boundary context-edit/compaction drafts, projection-aware summary preparation,
compaction interception, tree summaries or final Spark/Pi/SWE quality/release acceptance.
The [contract](context-edits.md) records the exact scope and current summary limitation.

Seven core tests cover duplicate text with distinct IDs, append-only raw records, branch checkout,
restore/omit behavior, whole call/result omission, reused call IDs in later units, reordered/split
multi-result records, missing-result uncertainty, protected source/call/image identity, bounded
proposals, scalar error payloads, summary raw anchors and guarded model admission. They check denial,
cancellation and post-adoption observation failure without provider IO. Three runtime tests cover
real latest edits in cold windows, raw archive pages, restoration/checkout, cold target validation,
read-only preparation and leases, window byte admission and changed-source conflicts before writes.

Two executable variants continue a full snapshot or a two-message archive window through the
ordinary Agent loop. Each makes one scripted model request containing the edited projection and
retains ten raw messages including the original target. They use no model credentials or external
network. CI runs both variants and the codec-free runtime suite.

Linux / Rust 1.92.0 passed **12 final local checks**, including all required checks,
with **882 default workspace tests, 1019 all-feature tests and
401 isolated core tests**. Default/all-feature Clippy and strict rustdoc passed,
as did the codec-free runtime suite, offline smoke and both executable variants. Commands/exits,
source identity and all measurements are retained in the [report](baselines/s7-context-edits-report.json).
These are local results; CI has not been observed here. No external model request was made.

All 272 resolved packages remain unchanged, as do manifests, lockfile and default features.
The no-edit projection path does not allocate a per-record edit index. The three cached release
samples pin source SHA-256 `43349cc74c78737ccb79d1f03e31d307fee960fc94d80a9a541a0021159094a4` and the parent revision; executable hashes are retained.

| Workspace-default observation | Median / size | Change from fork baseline |
| --- | --- | --- |
| Release CLI | 21.084 MiB | +1.397% |
| Release TUI | 21.254 MiB | +1.278% |
| Release SWE-bench | 20.238 MiB | -0.004% |
| 125-case smoke wall | 1104.12 ms | +180.254% |
| Smoke peak RSS | 9344.00 KiB | +0.000% |
| Idle offline runtime RSS | 4388.00 KiB | -1.260% |

Wall samples **1104.12 / 1283.85 / 1088.54 ms** are retained without filtering. Shared-host warm-cache timing
changes are observations, not causal performance claims or exclusion of smaller regressions.
Default smoke/idle do not execute active edits or rendered frontends; they do not bound edit
latency, archive/projection allocation peaks or isolated agent memory. Offline measurements do
not establish real-model quality, prices or billed usage. No standalone-eval isolation claim is
added by this increment.

The initial **+180.254%** wall observation triggered an independent investigation, rather than
being discarded as noise. [Source-bound matched builds](baselines/s7-context-edits-matched-report.json)
compile the parent and frozen candidate in separate owned checkouts and fresh targets. Compiler
artifacts bind all three workspace libraries and the example to their declared manifests; source
hashes were checked before/after builds and sampling. Ten alternating AB/BA pairs retain **2,500
cases**, with reference/candidate wall medians **401.90 / 398.05 ms** (-0.958%; paired median -1.111%).
These runtime-only builds omit the workspace's selected image codecs and have distinct binary
hashes. Both owned checkouts and target directories were removed afterward.

[Three later workspace observations](baselines/s7-context-edits-workspace-followup.json) retain
**399.51 / 400.50 / 390.17 ms** and all 375 cases, with the observed executable hash. No main
workspace rebuild ran between the initial and follow-up observations. These results do not reproduce
the initial large relative gap; the original slower values remain in the table/report. Shared-host
storage/timing variation has not been causally isolated, and smaller regressions are not excluded.
