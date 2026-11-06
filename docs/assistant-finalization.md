# Assistant message finalization

Native Rust hosts install `Extension::assistant_message_hook(HookSpec, Arc<dyn AssistantMessageHook>)`.
The reusable core executes hooks after a successful complete provider response and paid usage capture,
before canonical assistant persistence, tool dispatch, round-end actions or completion verification.
Lower priorities run first; ties retain installation/contribution order. No hooks are installed by
default and core acquires no new IO, dependencies or UI types.

`AssistantMessageInput` exposes workspace, round, original response, current ordered message,
response usage and stop reason. `None` preserves the current message. `Some(Message)` may replace,
add or remove Text/Reasoning blocks. The Assistant role and nonempty content are mandatory. Every
other block, including tool calls/arguments, images and opaque ProviderData, must remain identical
in relative order. Tool argument/result hooks retain their existing owned-plan authority. Visible
reasoning edits do not describe billed reasoning effort or grant extra model capabilities.

Each original/effective message is bounded to 1 MiB of serialized JSON; aggregate returned messages
are bounded to 4 MiB and a receipt to 64 KiB. The effective message plus receipt is charged to the
parent output budget. Registration shares existing bounded catalogs. HookSpec controls per-callback
timeouts (up to 30 seconds); each whole callback phase and journal wait is capped at 30 seconds.
Parent cancellation drops callback futures and expires scoped UI/child cancellation tokens.
Run duration is rechecked after callbacks and after acknowledged persistence.

An installed chain probes its journal before the first model request. A safe Started receipt names
installed hooks. Each successful response atomically records an Applied receipt and exactly one
effective assistant at its canonical index. Receipts contain round, serialized sizes, callback IDs
and change flags, never original message text or raw error diagnostics. None-returning callbacks
are named in Started; Applied lists returned proposals. Journal acknowledgement precedes effective
history, dispatch, verification and the `assistant_message_finalized` event. A successful but slow
acknowledgement remains a fact even if the parent duration expires immediately afterward.

Invalid edits, callback errors/panics, timeout or cancellation stop the current run with its paid
usage and original completed response retained in progress. Normal session finish preserves that
original when storage remains available. Persistence failure reports uncertainty; it never repeats
the provider request, dispatches the rejected edit or fabricates a successful finalization event.
Acknowledged edits participate in subsequent request projection/counting and canonical archives,
branch history, verifier candidates and exports. Restoring history never installs callbacks or grants.

Content/reasoning streaming events remain the raw provider preview. The authoritative finalization
event carries the acknowledged effective message and a changed flag. TUI replaces this response's
text/reasoning preview while retaining older responses and notices, including after bounded preview
eviction. Console and Web append a labelled final answer when changed; machine events retain their
declared JSON format. Print and final outcomes use canonical content. This API cannot redact bytes
already streamed to an observer.

Run the offline example with normal/denied tools or optional Code Mode:

```sh
cargo run --locked -p aicoder-runtime --example offline_assistant_finalization
cargo run --locked -p aicoder-runtime --example offline_assistant_finalization -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_assistant_finalization -- --code-mode
```

The example uses actual owned file tools, JSONL, bounded archives, inert HTML export and event
validation with no model/network credentials. `scripts/check_assistant_finalization.py` exercises
the native TUI fixture through a real PTY. See [acceptance](s7-assistant-finalization-acceptance.md).

This implements the assistant subset of Pi's message-finalization capability. Generic User/Custom/
Tool message transforms, session lifecycle/navigation veto and boundary context-edit/compaction
remain separate open work in the [audit](pi-parity-audit.md).
