# Raw user input and ordered submission

`Extension::input_hook(HookSpec, Arc<dyn InputHook>)` installs a host-owned raw-input stage.
It runs after frontend command dispatch and before prompt/skill expansion, turn validation,
persistence or steering notification. `InputAction` is Continue, Transform(AgentInput) or Handled.
Native code is explicitly installed by the host; settings, resources and history cannot install it.

Handlers run in HookSpec priority/installation order. Later handlers see the preceding effective
input. `AgentInput::with_prompt_text` changes only the first prompt block, retaining separately
captured file text and images. Returning a newly constructed AgentInput intentionally replaces
its complete message. User roles, text/image envelopes, sizes and final provider capabilities are
validated; provider routing, credentials, tools and approval rules are outside this action.

InputHookInput identifies delivery (Initial, Steer, FollowUp), workspace and origin (Sdk,
Interactive, Rpc, Web, Extension). Origin is informational host data, not authentication or
authority. A returned replacement cannot change the submitting origin. Stock hosts assign their
origin; native command Submit results receive Extension origin.

## Expansion and initial turns

Runtime installs its frozen ResourceSnapshot as InputExpander. Only the first prompt block is
expanded; immutable file pages and image attachments remain data. Expansion follows raw hooks,
so an extension can rewrite a command into a template invocation or consume an unknown command.
The same expander runs with resources disabled, preserving literal `//` and unknown-template
semantics. Plain core builders install neither hooks nor an expander.

Agent's text/multimodal entry points prepare fresh input once. They validate the effective
model view and atomically write its User message and safe receipt in the initial turn batch.
Existing recorded branch messages are unchanged. Runtime prefix/startup/request hooks run after
raw input and expansion. Raw ModelRequest/ExecutionInput histories are already explicit model
inputs; low-level executor calls do not guess which historical user message to transform.

Handled initial input returns AgentTurnOutcome::Stopped with RunStop::InputHandled and zero
model rounds. It creates no assistant answer or TurnStart/User/TurnEnd entry and does not recover
an earlier unfinished turn. Selecting New may already have created an empty session container.
RPC/Web report status `handled`; CLI succeeds without printing a model answer; TUI shows a notice.
The legacy into_result()/run() convenience requires a model answer and returns the typed handled
stop as an error. Use outcome APIs when extensions can consume submissions.

`TurnExecutor::prepare_user_input` is available for explicit SDK preparation. Ready carries a
private immutable marker bound to executor, run (when controlled), delivery and original start
time. Reusing it at the same boundary does not execute hooks/expansion again; another owner or
boundary rejects it. Agent consumes that marker automatically. Changing source/prompt text
explicitly starts a fresh preparation. Set source before preparing. History never restores
these markers, callbacks, UI handles, routing or grants.

## Controls without blocking host loops

Create owned controls through `Agent::control_channel` or `TurnExecutor::control_channel` to
freeze the input processor before exposing the handle. Raw inputs queued through an unbound
legacy channel before installing hooks reject the run before provider IO. Synchronous steer /
follow_up retain their prepared-message contract without hooks; installed raw hooks require the
asynchronous API and cannot be bypassed through those methods.

`RunHandle::begin_input(kind, AgentInput)` synchronously reserves order and capacity and returns
an owned InputSubmissionFuture. Poll/spawn it while continuing the frontend's event loop.
Dropping even an unpolled future releases its reservation. `submit_input(...).await` is the
convenience form; its reservation happens when that future is polled. Handlers are serial per
run in reservation order, independently of task scheduling. Existing steering priority and
model/tool delivery boundaries remain unchanged.

The result is Queued { input_id } or Handled { by }. Handled controls do not queue a model
message, signal steering, cancel an active tool or finish the current run. Processing failures
return RejectedInputSubmission with the original input; there is no fallback to raw input.
RPC reserves before spawning workers and matches deferred replies by request ID; replies can
interleave with events and later command replies. Status/cancel remain available. TUI keeps its
draft/attachments until acknowledgement, polls jobs alongside UI/events and preserves newer
edits. Run closure rejects late results and cancels private callback children.

Reservations count both in-flight and queued payloads. Raw and transformed messages obey the
channel's per-input limit; retained raw recovery data and receipts also count toward aggregate
queue capacity. Each raw/effective hook message is bounded to 2 MiB minus metadata headroom;
the transformation chain has a 16-MiB aggregate bound. The complete submission, including waiting
for an earlier reservation, has a 30-second ceiling and respects the remaining parent duration.
Preparation before the execution owner starts and cached preparation do not reset the run clock.
Hook timeouts/cancellation/error/panic sanitation reuse native extension scopes.

## History, receipts and budgets

`aicoder.input-hooks.v1` stores Started / Prepared records. Prepared contains origin, delivery,
actor/change flags, expansion flag and exact serialized raw/effective Message byte counts.
Those counts are not tokens, HTTP framing or billing. **Raw original text/images are never
serialized into receipts, events, history, exports or evaluation traces.** A transform used for
redaction therefore does not reintroduce the raw value through provenance.

InputReceipt.original and RunInput.provenance are ephemeral SDK recovery data with serde skip.
AgentInput::preparation_receipt and RunInput::original_message expose that owned in-memory data;
serialized controls omit it and deserialization cannot restore it. Effective user content remains
the canonical recorded message. Receipt size is bounded to 2 MiB; whole receipt output charges
the parent output budget before publication, together with prepared startup output. Token/window
admission counts the final expanded/effective request, using the normal shared counter/policy.

RunJournal adds record_input_hooks and atomic input_prepared. Unsupported journals reject
installed-hook execution before its first provider IO; producer-side input preparation may
already have occurred. Queued provenance and the effective user message commit together before
application; acknowledged facts survive elapsed-budget exhaustion. Failed checkpoints retain
pending SDK recovery data. Built-in session/evaluation journals and ephemeral () support the
contract. New runs and restored sessions never infer hook or approval authority from receipts.

Run the [offline runtime example](../crates/aicoder-runtime/examples/offline_input_hooks.rs):

```sh
cargo run --locked -p aicoder-runtime --example offline_input_hooks
cargo run --locked -p aicoder-runtime --example offline_input_hooks -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_input_hooks -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_input_hooks -- --code-mode --deny
cargo run --locked -p aicoder-runtime --example offline_input_hooks -- --handled
```

It exercises raw-before-template order, actual owned tools/approvals, JSONL, archives and inert
export with no external model. [Acceptance](s7-input-hooks-acceptance.md) also records RPC/Web
adapters and real terminal input UI checks. See [migration](migrations.md). Message transforms,
session navigation veto and boundary context-edit /
compaction drafts remain in the [Pi audit](pi-parity-audit.md).

[Custom triggers](custom-messages.md) are a separate canonical input type and skip this raw
user processing stage, including skill/template expansion.

[Busy custom delivery](custom-delivery.md) uses a separate typed queue path and does not run
raw user hooks or template expansion.
