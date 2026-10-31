# Coding extension examples (S6)

The examples compose existing ExtensionSet, hooks, commands, tools, Agent, sessions and event
contracts. No second model/tool execution loop or mandatory script runtime is introduced.
They run against temporary coding fixtures and scripted providers without credentials/network.

Planning is explicit host policy for one canonical workspace and one installed extension
instance. It appends context and rejects mutating/command capabilities before preparation or
approval-cache lookup. A wrapped prepared operation also checks the policy epoch. Mode changes
wait for admitted mutations, so enabling planning does not race a still-committing operation.
Policy state is ephemeral host state, not a restored authorization grant.

Protected paths are write-protected workspace prefixes. Both the requested lexical path and
the captured prepared targets are checked. PreparedToolEffects provides borrowed declarations
from the owned plan; unknown/opaque effects fail closed for mutation. Default file replacements
expose their captured targets while retaining their existing conflict checks, cleanup and
publication. The wrapper preserves the exact preview and owned plan across approval. Configured
prefix routes are pinned/checked; shell and unknown command effects are not inferred from text.
These controls apply to the installed tool set, not arbitrary same-user processes or native code.

Read-only workers are host-owned background Agent runs started by an extension command. Their
profiles fix provider/model, workspace, tool registry and finite round/tool/token/output/time
budgets. Task input selects a trusted logical profile and supplies data; it cannot choose
credentials, storage paths or tools. Workers receive fresh sessions and controls, no parent
history, run handle or approval grants. Parent IDs/branch/revision are correlation metadata,
never instructions or authority. Unknown/mutating/recursive tools are unavailable.

Each worker reports its own actual core outcome, usage/estimated token accounting and bounded
trace, and retains its canonical child session. This is command-level orchestration, not an
LLM delegate tool claiming to share a parent model run's budget. Start transfers ownership to
the host manager; cancelling an already delivered start command is not cancelling the worker.
The returned job ID supports status/cancel, and hosts await manager shutdown. Admission,
retention, observation pages and output previews are bounded; missing usage/trace delivery
cannot be reported as zero or complete. History/snapshots cannot recreate a live job.

## Run and adapt

```sh
cargo run --locked -p aicoder-runtime --example coding_extensions
```

The executable enables planning, denies a write, disables planning, approves a write once
for the live session, and proves planning still intercepts the cached grant. It then rejects
a protected path and a symlink alias, starts an explicit read-only review worker, checks its
actual JSONL lineage/outcome/usage/trace and verifies the parent history did not change.

The copyable implementations are `examples/support/coding_safety.rs` and
`examples/support/read_only_workers.rs`. Integration tests compile these same sources.
Install CodingSafety as a context hook, tool hook and plan command; wrap every admitted
mutating tool. Keep the resulting ExtensionSet immutable and install no unwrapped alternate
writer/shell tools. SDK/CLI/Web/TUI hosts use the existing extension installation APIs.
This example adds no default tool or automatic policy to Runtime/TurnExecutor builders.

## Prepared effects and protected routes

`PreparedTool::effects()` defaults to `PreparedToolEffects::Unknown`, preserving existing
implementations. `FileMutation { paths }` borrows captured absolute paths from the owned
operation; it is not a model-controlled JSON annotation or a replacement for approval.
The default write/edit FilePlan returns its captured target. A guard must trust the registered
tool implementation to declare all effects, retain the exact same plan and fail closed for
unknown effects. There is no path guessing from diff text or shell command parsing.

The policy checks a bounded normalized request path, then the owned concrete effects.
A symlink into a protected prefix is rejected after read-only preparation and before any
approval/cache lookup. A later retarget still fails the original file backend's conflict
checks. The wrapper returns the original preview and consumes the original operation once.
Planning's epoch check rejects stale preparations, including an enable/disable cycle; its
read lease is held throughout commit, so an explicit mode change waits for admitted work.

Protected configuration pins existing route descriptors and remembers missing components.
Known route replacement/creation causes policy_stale and requires explicit host replacement.
Limits are 64 prefixes, 8 KiB aggregate names, 128 aggregate components and at most 32 captured
targets; paths are at most 4 KiB. Configured links/special files fail construction. Policy
state is scoped to the canonical workspace, not restored from saved domain history. Same-user
processes can rename/write outside participating tools; protection is a tool policy, not an
OS filesystem boundary. External changes after a policy check retain that limitation.

## Worker profiles, ownership and storage

WorkerProfile is assembled by the host with fixed model/provider/registry and a finite
WorkerBudget. It accepts at most eight trusted read-only definitions (32 KiB aggregate).
Rounds are 1–8, tool calls 0–16, tokens 128–20,000, output 1–128 KiB and duration 1 ms–120 s.
Model output preference is 1,024 tokens; known windows and core preflight remain authoritative.
Each task is nonempty and at most 8 KiB. Child runs have no default tools, nested worker tool,
parent history, approval grants or inherited run controls. A read-only capability is a trusted
host declaration, not proof that arbitrary native Rust cannot perform IO.

A manager admits 1–4 fixed profiles, 1–4 concurrent jobs, eight retained jobs and 32 lifetime
starts. Saturation returns an explicit error without creating a queued model run. Forget is
allowed only after owner completion; canonical child history is retained. The lifetime cap
bounds newly created sessions per manager; a long-lived application must separately manage
storage across explicitly created manager generations.

The `worker` command supports typed start/status/list/cancel/forget actions. Unknown routing/
workspace/credential fields fail extraction. Start returns a job ID and explicitly transfers
lifecycle ownership to the host manager. If start delivery is interrupted, inspect the retained
job list rather than repeating a potentially committed start. Cancel the job by ID; closing
admission and awaiting `shutdown()` cancels/joins all owners. Dropping the manager signals
cancellation; queued unpolled task drops publish uncertainty synchronously without scheduling
another cleanup task. An observer/metadata snapshot cannot recreate a job after restart.

The supplied repository is trusted private host storage. Each new child session records
`example.worker_origin.v1` with job/profile/run ID and optional parent session/leaf/revision.
The unknown extension record is correlation data, not projected instructions or authority.
The actual Agent writes canonical child messages/checkpoints/outcomes through the same
repository protocol. No grading answers or hidden evaluation fixtures are used as input.

Reports contain the actual core stop, rounds, calls, elapsed time, usage and estimated-token
precision, plus a 4 KiB final preview. Missing outcomes leave usage/tokens unknown. At most
2,048 event frames and 1 MiB serialized trace are retained per job; capture/transport gaps
prevent trace_complete. Pages contain at most 50 complete frames and 64 KiB; an oversized
next frame fails explicitly. Offset/run/job identity remain explicit and no event is replayed
as a new external operation. The canonical archive is independent of preview/trace retention.

These independent reports are suitable for SDK benchmarks. They do not pretend child usage
is a charge already included in an unrelated parent model turn. A future LLM delegate tool
would need a shared parent budget/accounting protocol; this host-command example deliberately
has no hidden parent model/tool execution or fabricated aggregate cost.
