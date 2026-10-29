# Event protocol v1 and bounded observation

Core now publishes one canonical `AgentRawEventEnvelope` for SDK observers, TUI and
benchmark artifacts. [JSON Schema](protocol/events-v1.schema.json) defines the writer
format; a [normalized complete trace](protocol/events-v1-trace.jsonl) is checked against
actual SDK output. This is an observation protocol. CLI stdin/stdout RPC commands and
Web/SSE servers remain S5 work, and event delivery is not execution replay.

## Identity, time and lifecycle

Each frame contains `schema_version: 1`, a UUID `run_id`, a contiguous unsigned
`sequence` starting at 1, `elapsed_ms`, optional round/session/turn identity, and an
`event` object discriminated by `type`. Producer time is measured from AgentStarted
with a monotonic clock. Round indices start at 1. Existing retry `delay` values retain
Serde's lossless `{secs,nanos}` Duration form. Typed callback metadata carries the same
identity/time and optional terminal summary; metadata types are now Clone rather than Copy.

A complete run has:

1. AgentStarted and the Preparing state transition, including on preparation failure.
2. For session runs, one SessionBound after workspace validation. Earlier frames have
   no session identity; all later frames carry the same session/turn. The turn ID
   identifies an attempt; a preparation failure can precede a durable TurnStart.
3. Ordered RoundStarted / RoundCompleted pairs with their round IDs. A round that
   stops early is closed before terminal state. Budget/model stops have a typed
   RoundOutcome::Stopped; cancellation and failures retain their own classification.
4. One terminal state followed by exactly one AgentCompleted, AgentStopped,
   AgentAborted or AgentFailed. That frame seals publication. No later model payload
   is accepted, even if a custom provider retained its sink.

Terminal `termination` metadata includes the typed stop, optional secondary persistence
failure, usage, rounds/tools/output/tokens, execution elapsed milliseconds and pending
input IDs/kinds. It excludes full transcript bodies and queued input text. The complete
result/progress and session remain authoritative. Execution elapsed time excludes
session preparation/final persistence and callback drain; envelope elapsed time includes
the run lifecycle up to publication. Execution time uses Tokio's budget clock, which tests may pause/advance independently of real producer time.
These clocks deliberately measure different scopes and should not be subtracted as
though they were interchangeable.

`EventTraceValidator` validates a **complete** trace: version, sequence, stable identity,
state transitions, round pairing, terminal summary consistency and absence of later
frames. Once rejected, a validator remains rejected. `finish()` rejects incomplete
prefixes. Deserialization rejects unsupported versions, invalid UUIDs/zero sequence or
round, and unknown event variants. Unknown additional fields are accepted; changes to
existing meaning/required shape or new event variants require a new protocol version.
This v1 format does not adopt Pi's TypeScript event ABI.

## Subscribe independently of execution

```rust,ignore
use aicoder_core::{RunHandle, events::EventBufferConfig};
let (handle, control) = RunHandle::pair();
let mut events = handle.subscribe(EventBufferConfig {
    max_events: 1024,
    max_bytes: 4 * 1024 * 1024,
})?;
let running = tokio::spawn(async move {
    agent.run_controlled("inspect the project", callbacks, control).await
});
while let Some(frame) = events.recv().await? {
    // Serialize the typed envelope; do not parse its Debug representation.
    println!("{}", serde_json::to_string(frame.as_ref())?);
}
let outcome = running.await?;
```

`RunHandle::events()` exposes the shared EventSource; the unique RunControl can also
subscribe before execution. These consumers use no callback thread. EventSource
retains only configured queues and delivery metadata, not a global replay log.

Each queue is bounded independently by frame count **and serialized JSON bytes**.
Defaults are 1024 frames / 4 MiB. Byte accounting includes envelope metadata and JSON
escaping, but excludes JSONL newline/transport framing and allocator overhead. A
callback in flight and frames already handed to consumers are outside queued-byte
accounting. Frames
are shared through Arc across subscribers. A counting writer computes size without
allocating another complete serialized frame. New subscription registrations are capped at 16 per
run, plus the optional callback adapter, including prior disconnected subscriptions; retained reports therefore stay bounded.
Both buffer limits must be positive. Hosts choose appropriate limits for their workload.

The synchronous producer never waits for observers. On overflow the affected observer
is detached and keeps only its already queued contiguous prefix. It receives that prefix,
then **one explicit EventStreamError**, then EOF. The error reports Lagged or
EventTooLarge and its first missing / last delivered sequence. Payloads are not silently
truncated, coalesced or replaced. Other observers and the model/tool loop continue.
Dropped receivers clear their buffers and report ClientClosed. Dropping the execution
owner before terminal publication reports Abandoned to active observers, discards any unconsumed prefix
with an explicit error, and does not invent a completed run.

Subscriptions registered mid-run begin at `starting_sequence` in their delivery report.
They observe a suffix and cannot claim complete history. A closed source rejects new
subscriptions. There is no cursor resume, reconnect replay, or guarantee of delivering
a terminal frame after an overflow/disconnect. Use the run result/session to recover.
Dropping a subscription or host handle does **not** cancel the agent: cancellation is
an explicit RunHandle/context action. Hosts own tasks, joining and shutdown policy.

## Callback compatibility and delivery reports

The existing AgentEventHandler API remains available. Core gives each real callback
one bounded queue and a dedicated serialized worker. `()` explicitly requests no
callback observation, so it allocates neither callback queue nor thread. A callback
panic detaches it and records HandlerPanicked; worker startup/loss is WorkerUnavailable.
No user callback executes on a Tokio worker.

Shutdown seals publication and drains callbacks for `EventDeliveryConfig`'s configured
period (default one second). This deadline uses **real time**, separately from Tokio's
execution budget clock; paused/virtual execution time must not truncate healthy OS
callback workers. The wait runs on the blocking pool only if completion is not already
ready. Expiry discards queued callbacks and records CallbackTimedOut. Acknowledgements
from that discarded callback are no longer counted. Rust cannot
preempt an arbitrary synchronous callback already executing: at most that in-flight
callback may finish later; no subsequent callback starts. Blocking callbacks should use
subscriptions or move their own work out of the callback. Input/approval policies use
their dedicated core interfaces, not an event callback timing assumption.

`RunProgress.event_delivery` reports observer status, IDs, starting/last sequences,
first missing sequence and peak/current queue counts/bytes. It does not change the
primary run stop or undo tools. It is a **snapshot at return**: ordinary asynchronous
subscriptions may still be Draining, and their later status is available through
EventSource/receiver reports. A journal's finish sees the earlier pre-terminal snapshot;
it cannot know subsequent drain/consumer behavior. Session terminal records do not
claim to store final observer acknowledgements. `has_failures()` includes explicit
client disconnects as well as overflow/worker errors.

CLI prints incomplete live-delivery diagnostics. TUI uses the bounded subscription
directly and a single completion oneshot, removing its unbounded forwarding channel.
If observation becomes incomplete, TUI reports it and reconstructs the timeline from
the complete run result, preserving the input draft. A task that dies without a result
is shown as failed; its recorded session can be inspected/recovered. TUI exit still
explicitly cancels by product policy. These queue bounds do not bound the entire
persistent session, returned transcript, rendered history or an application's own
archive of received events.

## Bench artifacts and migration

Evaluation `trace.json` is now an array of canonical envelopes with structured `event`
objects, replacing Debug strings and the redundant kind column. The runner validates
the complete trace and marks incomplete observation as an evaluator error instead of
claiming a complete trajectory. SWE-bench reports use schema 3 with
`event_schema_version: 1`; checkpoints without that version are not resumed into a new
run. No in-place conversion of historical traces is attempted. A rerun/resume into the
same output directory regenerates incompatible case artifacts; copy that directory
first if the old artifacts must be retained. Legacy artifacts are identified by
their missing event schema. See [API migration notes](migrations.md).

The queue overflow policy applies to observations. It does not suppress durable tool
checkpoints or final run/session data; see [sessions](sessions.md) and [controls](run-control.md).
A process crash can still prevent final persistence, and no event queue reconstructs
unrecorded external side effects.

## Offline verification and schema tooling

```sh
cargo run --locked -p aicoder-core --example event_stream
cargo run --locked -p aicoder-core --example event_stream -- --overflow
```

The first prints canonical JSONL using a memory session and a scripted provider.
The second intentionally overruns a one-frame subscription: it reports the gap while
the agent still completes. Both use no credentials, model network or file mutation.

The schema checker is development/CI-only; Rust binaries need no Python:

```sh
python3 -m venv /tmp/aicoder-schema
/tmp/aicoder-schema/bin/python -m pip install -r scripts/requirements-schema.txt
/tmp/aicoder-schema/bin/python scripts/check_event_schema.py
```

Pass additional JSONL paths to validate fresh example output. CI checks the published
fixture with JSON Schema, while Cargo tests compare it with real SDK output and
exercise deserialization, lifecycle, limits, concurrent producers, disconnects,
callback failures/real-time drain, cancellation, task destruction and TUI reconciliation.


S3 adds optional prepared-tool previews to `tool_approval_requested` and controlled
approval invocations. The version stays 1; absent previews remain valid. The
[approval trace](protocol/approval-preview-v1-trace.jsonl) contains full success/conflict
runs, and the default schema checker validates it alongside the original fixture.
See [prepare/review/commit](file-mutations.md) for limits and source-data handling.


Session authorization adds optional scope/resolution metadata to approval records,
while retaining event version 1 and the existing approved boolean. The default schema
checker now includes [creation/reuse/revocation/denial](protocol/session-approvals-v1-trace.jsonl).
[Session authorization](approvals.md) describes source attribution and validation;
serialized events and snapshots never restore permission state.


S4 adds optional terminal `model_context` metadata (source branch, initial/view counts,
logical request attempts, last adapted request estimate and compaction count). Older
terminal data remains readable. The validator checks these counts against observed events.
The [branch projection fixture](protocol/model-context-v1-trace.jsonl) exercises an
unbound SDK branch run with model-only repairs; see [the context contract](model-context.md).
