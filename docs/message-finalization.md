# Fresh-message finalization

Register `Extension::message_hook(HookSpec, Arc<dyn MessageHook>)` in an explicit native host.
Each chain runs in ascending priority, with installation order resolving ties. It receives only
a fresh message entering canonical execution, never restored history or a provider-only repair.
Returning None preserves the current value; later hooks see earlier proposals alongside an
immutable original. Custom metadata remains immutable. Core adds no HTTP, storage backend,
dependencies or default callbacks; disabled chains allocate no additional callback object.

| Origin | Point of finalization | Atomic adoption |
| --- | --- | --- |
| Initial User/Custom | After raw input hooks and expansion, before startup/context admission | Existing initial turn batch; transient progress for sessionless Agent |
| Steer/FollowUp | When the existing control loop consumes the queued input | Effective input, existing provenance and finalization receipt |
| Deferred Custom | After paired results / final candidate checkpoint | Custom context and receipt, without forcing another model request |
| NextTurn Custom | When an explicit trigger claims the inbox in its target run | Initial batch; consume claim only after acknowledgement |
| Idle Custom | Under the existing idle session lease, before compatibility validation | Custom entry and receipt |
| Assistant | After paid usage capture and specialized AssistantMessageHook callbacks | Effective assistant and both stages' receipts, once |
| Tool | After specialized ToolResultHook/output normalization, before canonical checkpoint | Effective result and receipt, with original completion retained until acknowledgement |
| Startup/Boundary System context | After proposals are staged, before admission | Existing typed native-context transaction and receipts |

AgentPrompt entry points identify fresh user/custom messages. Low-level ModelRequest and
ExecutionInput carry caller-supplied histories; their existing entries are not implicitly fresh
or re-finalized. Embedders, including benchmarks that need initial-user hooks, use AgentPrompt
entry points. Bare executors still finalize their newly generated assistant/tool messages and
consumed controls. No second execution loop or terminal parsing is required.

User edits must form valid bounded AgentInput, including supported immutable image envelopes;
capabilities and known context windows are checked before initial session writes/provider IO.
Custom edits retain source/type/display/details and their exact source label. Sourced System
context retains its native source and one-text-block form; its body remains subject to context
entry limits. Runtime instruction prefixes are managed by BeforeStartHook and are not messages
offered here. Neither metadata nor a source-shaped string grants executable authority.

Assistant edits preserve role, tool calls, images and opaque provider state. Generic tool edits
preserve call IDs, delivered error status, error code/details, capture fields, interception metadata
and typed media/artifact references. They may replace the success output or error message. Use
ToolResultHook first for deliberate delivered-status changes or artifact narrowing, with its
immutable actual-execution receipt. ToolCallHook continues to own argument changes before guards,
preparation and approval. Finalization cannot edit approved plans or repeat completed effects.

Original/effective messages are bounded to 2 MiB serialized JSON; returned proposals total at
most 8 MiB per chain, and one receipt at most 64 KiB. Existing custom/image bounds, tool output
limits, context entry/aggregate limits and parent output/duration budgets still apply. The older
AssistantMessageHook phase retains its own 1 MiB limit. Each HookSpec timeout and whole chain are
bounded to 30 seconds. Cancellation/drop expires private callback/UI children; duration is checked
after callback work and acknowledged writes. Payloads/receipts are admitted as complete values.

RunJournal support is probed before model IO. Initial session writes and context commits preserve
their existing transactions; dispatched messages use MessageFinalizationCommit. Receipts record
origin, role, round, sizes and callback change metadata, without original text. A failed initial
hook writes no turn; a failed queued hook leaves its input recoverable. Failed/cancelled tool or
assistant finalization retains the completed original and paid usage. Failed receipt publication
never retries provider/tool effects or emits a successful finalization notification. A slow
acknowledgement remains a fact even when the duration budget expires immediately afterward.
Missing/corrupt storage remains explicit; an in-memory result does not assert successful storage.

The message_finalized event follows adoption and carries effective content, origin, change flag
and optional custom metadata. AssistantMessageFinalized is also emitted for compatibility: choose
one for assistant display. Stock TUI replaces the assistant preview and labels other changed
messages; console/Web label effective messages. Hidden custom content stays hidden. Prior stream
chunks and tool observations remain previews; no redaction of already delivered data is promised.
Canonical history, subsequent model projections, archives and exports use the effective value.

```sh
cargo run --locked -p aicoder-runtime --example offline_message_finalization
cargo run --locked -p aicoder-runtime --example offline_message_finalization -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_message_finalization -- --code-mode
```

These examples use actual owned file tools, JSONL, bounded archives and inert export with scripted
models. Core tests cover idle/queued/inbox/context/recovery boundaries, and Web/TUI tests exercise
authenticated SSE and real terminal delivery. See [acceptance](s7-message-finalization-acceptance.md).
Session lifecycle/navigation veto and context-edit/compaction drafts remain open in the
[Pi audit](pi-parity-audit.md); this API does not claim identical TypeScript ABI or failure policy.
