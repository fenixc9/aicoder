# RunHandle: bounded inputs, cancellation and approval

`RunHandle` is the cloneable host side of one run; `RunControl` is its unique,
non-cloneable execution side. Move the control into `TurnExecutor::run_controlled`,
`run_controlled_with_journal`, `Agent::run_controlled`, or
`Agent::run_with_session_controlled`. Existing context-only methods keep their
behavior and do not allocate a control queue.

```rust,ignore
let (handle, control) = RunHandle::pair();
let running = tokio::spawn(async move {
    agent.run_with_session_controlled(
        repository.as_ref(), selection, "inspect this project", events, control,
    ).await
});
handle.steer(Message::text(MessageRole::User, "Focus on the failing test first"))?;
handle.follow_up(Message::text(MessageRole::User, "Then explain the changes"))?;
// handle.cancel("user requested stop")?;
let outcome = running.await?;
```

Hosts can supply a parent cancellation context and configuration through
`RunHandle::channel(parent, config)`. The run receives a child context: cancelling
this handle does not cancel the caller or unrelated runs. Dropping only host handle
clones does not cancel execution. Dropping the unique control/future closes controls,
releases pending approval senders, and propagates cancellation to its child work.
The host owns task spawning, joining and shutdown; core does not spawn the run itself.
Await the run future before starting another run on the same session: handle status
does not join the task, and the session lease is released during Agent cleanup.

## Input semantics

`steer` and `follow_up` return a run-local monotonically increasing input ID.
Their `Message` must be user text/images; system, assistant, tool, reasoning and
provider-state blocks cannot be submitted through controls. Actual model capability
validation still occurs before model calls. An accepted message is queued in memory;
acceptance does not assert that the model has consumed it or that it is on disk.

- Steering is FIFO within its own queue and has priority over follow-ups. It is
  applied before the next model round, or after a verified completion before another
  round. A pending model request, compactor or verifier runs to its current safe
  boundary. Steering does not forcibly terminate these stages.
- When steering arrives during tool dispatch, an already executing tool finishes.
  Calls not yet started receive recorded error results with code
  `steered_before_execution`. Waiting approval is a safe boundary: steering withdraws
  it without executing the operation. Results remain paired in original call order;
  the next request includes the steering input. Concurrent read-only operations that
  have already started may finish. Steering never rolls back an external mutation.
- A follow-up waits for an accepted completion, then enters the transcript as a user
  input and starts another model round. One follow-up is consumed per completion;
  queued steering is considered first. The prior candidate remains in history.
- All of this is **one run**, with one round/tool/token/time/output budget. A queued
  follow-up does not reset budgets. At the last permitted round it stays pending and
  the run stops with a round-budget outcome instead of silently discarding it.

Only a bounded number of inputs are applied at each boundary, so a host continually
submitting steering cannot keep the executor in an unbounded drain loop. Applied
inputs are added to `RunProgress.messages`; `RunInputApplied` is emitted after their
journal checkpoint succeeds. Here "applied" means included in the transcript, not
answered by a model: a later budget/provider failure can still prevent a model call.

Default limits are 32 pending messages, 64 KiB serialized JSON per message and
256 KiB total pending JSON. Both count and byte limits apply, including an input
whose checkpoint has not completed. The size check counts through a bounded writer
without allocating another complete serialized string. Configuration limits must
be positive. Rejections return `RejectedRunInput`, containing a typed error and the
original message, so a frontend can preserve the draft. Queue bytes/count are freed
only after successful application.

The completion boundary atomically checks pending input and closes acceptance.
Once finishing/finished/dropped/cancelled, submissions and approval responses are
rejected. Inputs cannot be accepted after the final empty-queue check and disappear
between that check and the returned outcome. Cancellation, budget or failure closes
acceptance and copies unconsumed messages into `RunProgress.pending_inputs`. The
handle snapshot also retains them. A host must explicitly decide whether to resubmit
these inputs; recovery does not execute them automatically.

## Asynchronous approvals

Normal runs continue using the configured `ApprovalHandler`, with deny by default.
Handle approvals require explicit opt-in:

```rust,ignore
let (handle, control) = RunHandle::channel(
    TurnExecutionContext::new(),
    RunControlConfig { handle_approvals: true, ..Default::default() },
)?;
// Run control on the execution task. Observe on the host task:
loop {
    let snapshot = handle.snapshot();
    if let Some(request) = snapshot.approvals.first() {
        handle.respond_approval(&request.id, true)?; // or false
        break;
    }
    // Stop on finishing/terminal/cancelled state according to the host lifecycle.
    handle.changed(snapshot.revision).await;
}
```

An approval ID contains run identity and a unique sequence, independent of model
call IDs. A model reusing `call_id` cannot make an old approval apply to a later
operation. Foreign, already answered and expired requests fail. Snapshot lookup and
`RunApprovalRequested` events expose the exact invocation. A decision is a single
oneshot response; no session-wide grant is created. Rejection feeds an approval-denied
tool result to the model. Handle mode replaces the configured handler only for this
explicitly configured run. Read-only tools still need no approval.

Cancel, steering while waiting, timeout/budget and task destruction clean up approval
state. Late decisions cannot authorize a new invocation. Core permits one pending
mutating/command approval because its existing dispatcher serializes those tools.
Hosts should configure a time budget or their own cancellation policy if they do not
want an unanswered approval to wait indefinitely.

## State, events and persistence

`RunControlSnapshot` reports run/session identity, revision, status, pending inputs
and approvals. Session identity is bound when the session-aware agent selects its
session; stateless runs have none. `changed(revision)` waits without polling or missed
notifications for a newer control revision, parent cancellation or terminal state.
Finished and dropped snapshots return immediately; cancellation may be observable
with the same revision as the prior snapshot. Status becomes finished before terminal
run callbacks execute. Status `Dropped` means the execution owner was destroyed and
may have no durable terminal record.

The existing event envelope uses the handle's run ID. `RunInputApplied` and
`RunApprovalRequested` have typed callbacks as well as raw events. Full event schema,
versioning and bounded observation are the remaining S2 work; the old event transport
and TUI event channel are still unbounded in this increment. `RunHandle::changed` is
a control-state observation API, not a replay log or event delivery guarantee.

`RunJournal::input` defaults to `checkpoint`. The bundled session journal overrides
it to atomically append provenance (`aicoder.run.input` extension: run/id/kind) and the
user message, without copying the entire message into the provenance JSON. Candidates
are saved before any subsequent control message so history order is preserved. Idle
controls do not force an extra candidate write on a normal successful session turn.
Terminal records save pending inputs alongside stop, partial response and statistics.
`pending_inputs` has a serde default, so earlier v3 session records remain readable.

Failure of an input checkpoint retains the message in progress and pending state and
prevents the next model request. As with other uncertain writes, callers should inspect
the reopened log rather than blindly retry. Queues are not durable mailboxes: a process
crash before application/final persistence can lose in-memory accepted inputs. A dropped
task leaves pending inputs available while its handle is alive, but cannot write a final
record asynchronously from Drop. Hosts needing durable submission receipts must persist
their inbox before submission. See [session recovery](sessions.md) for uncertain tool effects.

## Frontends and offline example

TUI now uses the core handle for approvals and control. While a run is active, type
in the input box and press Enter to steer or Alt+Enter to queue a follow-up. Esc/Ctrl-C
still cancel. Approval modals keep their y/n decision UI. Rejected submissions retain
the input text; queued and applied inputs have distinct timeline notices, and pending
inputs are shown on interruption/session reload. SDK hosts can deliver controls during
approval without using the modal. Full terminal history/multiline and CLI RPC controls
remain S5 work.

```sh
cargo run --locked -p aicoder-core --example controlled_agent
```

This example approves an in-memory tool and processes a follow-up in three model
rounds. It requires no model credentials, network or file mutation. The integration
suite covers queue priority/bounds, mutation barriers, steering during model/approval,
scoped approval IDs, cancellation/drop, budget carryover, failure retention,
terminal-callback closure and memory/JSONL persistence order.
