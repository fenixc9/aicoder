# S7 general fresh-message finalization acceptance

MessageHook covers fresh User/Custom/Assistant/Tool messages and explicitly sourced System context.
It composes after raw input/expansion and specialized assistant/result hooks, preserving actual
execution, sources and typed media. Existing histories and runtime prefixes are not re-finalized.
See [contract](message-finalization.md) and [migration](migrations.md).

Seventeen core tests cover initial input and canonical once-only storage; idle/custom triggers,
next-turn claims, hidden details, restored-history separation; user/custom steer and follow-up;
deferred context after the final candidate without another request; startup/boundary provenance;
ordered chains; raw-hook/expansion order and private-original omission; capability/window/output
admission before writes; unsupported/failed journals; callback and capture-field tampering; tool
output bounds; cancellation/child expiry; failed tool receipts and acknowledged writes whose
duration expires. Specific result edits run before generic finalization while retaining actual
execution status. Completed effects/paid usage are preserved without repeating operations.

Two runtime tests and four executable allow/deny variants use actual owned file tools in normal
and Code Mode. Each uses two model requests / 14 fixture tokens, two assistant-specific calls and
four general calls, with exactly two canonical assistants. The first request sees the finalized
user and the second sees the finalized tool payload. JSONL, bounded archives, inert export and
event validation pass. Code Mode leaf facts remain distinct from the root Tool message.

A real PTY verifies one effective User and one Assistant, no private original input/assistant
preview in canonical history, safe receipts and visible final output. A TUI test protects hidden
custom display. An authenticated loopback Web test checks authorization before execution, the
effective model input, both finalization SSE frames and absence of raw input in those frames.
Other extension UI tests remain green; this is not a dedicated browser-rendering test. Schemas
reject custom metadata/role and origin/role mismatches. Old assistant-only PTY, input/custom,
frontend, transport, image, benchmark and SWE bridge checks also pass.

Rust 1.92.0 / Linux passed **99 local final gates: 840 default workspace tests,
973 all-feature tests and 375 isolated core tests**. Codec/JS-free runtime selects
1 finalization test; Code Mode adds the second. Exact commands/exits are in the
[report](baselines/s7-message-finalization-report.json). Workspace fmt, default/all-feature Clippy,
core/workspace tests, offline smoke and strict rustdoc passed. CI adds all four examples and the
PTY check. These are local results rather than hosted CI. There are no new external model calls.

Session versions, dependencies, manifests, lockfile and default features are unchanged. The new
journal contract and BoundaryCommit field are documented; inert receipts restore no callbacks,
queues, routes or authorization. AssistantMessageFinalized remains a compatibility event. Generic
tool status/artifact edits use the existing specialized APIs; finalization protects execution
facts. Bare low-level histories are preserved, with AgentPrompt available for benchmark hosts
needing fresh-input hooks. Native failure policy/bounds and TypeScript ABI differ from Pi.
Session lifecycle/navigation veto, context-edit/compaction drafts, final isolated Spark/Pi and
real SWE quality/release acceptance remain open in the [audit](pi-parity-audit.md).

Three cached workspace samples pin source SHA-256 `d741f68a75022051b1d6088e408625a4d08ee5a79dc00b6ec3ffc96915ad1fa5`; parent revision
identifies the checkout and the digest pins the implementation. All 272
resolved packages are unchanged. Default smoke/idle do not install these callbacks or render
frontends; they do not measure active-hook memory or isolate a performance regression.

| Workspace-default metric | Median / observation | Change from assistant-finalization baseline |
| --- | --- | --- |
| Release CLI | 20.175 MiB | +1.551% |
| Release TUI | 20.237 MiB | +1.671% |
| Release SWE-bench | 20.209 MiB | +1.135% |
| 125-case smoke wall | 979.88 ms | +145.723% |
| Smoke peak RSS | 8,960.00 KiB | +2.941% |
| Idle offline runtime RSS | 4,368.00 KiB | -1.533% |

Wall samples **979.88 / 393.38 / 1036.88 ms**
are retained without filtering. Shared-host warm-cache observations establish no causal speed,
cold-build or allocator claim. Standalone eval release is **19,988,184 bytes /
19.062 MiB** (+1.237%), with no selected codec,
QuickJS or auxiliary-model dependency. Offline observations do not close real-model comparison gates.

## Performance follow-up and evidence correction

The workspace wall median rose **145.72%** against the preceding sample. All three observations
remain above: 979.88 / 393.38 / 1,036.88 ms. Sample 0 retained a 572.979-ms SessionResume; sample 2
retained a 385.443-ms Edit and 256.300-ms SessionResume. Scenario medians stayed around 3.84–4.09 ms
for Edit and 9.37–10.12 ms for SessionResume. These observations require further comparison and
are not discarded as noise.

The [supplementary report](baselines/s7-message-finalization-matched-report.json) freezes clean
reference source 191fd80 and the candidate implementation digest, builds in **two independent target
directories**, verifies fresh compiler-artifact manifests for core/providers/runtime and the example,
and freezes two distinct executable hashes. Both source digests match their workspace-baseline
reports. Ten alternating AB/BA pairs run 125 scripted cases per process: 2,500 cases total, all retained.
Owned temporary worktrees and build directories were removed after measurement.

| Matched standalone observation | Reference | Candidate |
| --- | --- | --- |
| Process wall median | 398.92 ms | 399.37 ms |
| Recorded case total median | 368.57 ms | 371.25 ms |
| Edit median | 3.949 ms | 3.896 ms |
| SessionResume median | 9.823 ms | 9.652 ms |

The ratio of process medians is **+0.11%**; median paired change is
**+1.12%**. The large magnitude from the three workspace
samples was not reproduced here. This does not exclude a smaller regression, prove causality or
measure active hooks, isolated peak RSS, real-model quality or billing. Standalone feature selection
and shared-host warm-cache conditions differ from the original workspace measurements.

This review also corrected an earlier [performance follow-up](s7-terminal-exit-followup.md): its
requested candidate source was not independently bound to a newly compiled executable. Both old
hashes were identical and its candidate build showed no recompilation. Historical samples are
retained with the revision-comparison claim marked unproven. The new comparison verifies both
fresh source bindings; it does not retroactively validate the old selected revisions.
