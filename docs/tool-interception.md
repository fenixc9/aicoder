# Native tool argument and result interception

Explicit Rust extensions can normalize tool arguments before owned preparation/approval and
chain transformations of delivered results. ToolCallHook and ToolResultHook complement the
existing read-only ToolHook gate. Ordinary calls and composed Code Mode leaves use the same
dispatcher; no new execution loop, native-code loader or persisted authority is introduced.
Lifecycle actions and provider/request transformations remain separate open Pi-audit items.

## Registration and order

An Extension registers tool_call_hook(HookSpec, Arc<dyn ToolCallHook>) and
tool_result_hook(HookSpec, Arc<dyn ToolResultHook>). ExtensionRegistry freezes the selected
native callbacks. Lower priority runs first; equal priorities retain installation/contribution
order. Native implementations are trusted host code with Send + Sync callbacks, not a sandbox.
Each callback uses the existing 1 ms–30 s cooperative HookSpec deadline and private cancellation
observation token. Each chain and journal await is additionally bounded by tool_timeout and
parent cancellation; steering supersedes a chain. Callback failures expose owner/hook and
failure class, not raw native error strings. Dropping a callback cancels its private children.

The execution order is:

1. Admit the tool call, resolve the frozen tool and reject unknown/unadvertised root calls.
2. Run ToolCallHook transformations in order; each receives original and current call data.
3. Run existing ToolHook guards on the effective arguments.
4. Prepare once, apply prepared-effect guards, build the owned preview and request approval.
5. Commit ResultPending when result interception is installed; recheck the approval ticket,
   steering and cancellation after this wait, then invoke the same owned operation once.
6. Normalize/bound/archive raw returned output, then run ToolResultHook transformations.
7. Record completion/transform receipts, account parent output, publish the delivered tool
   event and checkpoint the paired result in ordinary canonical call order.

Guard/approval/preparation failures are paired results and are not passed to result callbacks;
a result callback cannot turn denied authorization into execution. Cancellation can leave a
Pending record without Finished. Such a record proves intent, never that an operation began,
completed or can be retried. An expired ticket cannot regain authority while receipts are saved.

## Argument transformations

ToolCallHookInput exposes workspace, immutable original/current ModelToolCall and the frozen
tool capability. Return Continue, Arguments(JSON object), or Deny { reason }. Names, call IDs,
capabilities, tool exposure, provider routes and approval policies are not writable fields.
Later transformations see earlier arguments; existing guards and preparation see the final
version. Approval and cached-grant lookup use its owned invocation/preview. Explicit session
grants retain their documented tool-wide scope; transformations do not create grants and guards
still run before a cached grant can apply.

Original arguments and each replacement admit at most 64 KiB serialized JSON; aggregate
replacement payload is at most 256 KiB per chain. Denial reasons admit 4 KiB. Non-objects,
excessive edits or callback failures stop before preparation/execution. Each accepted proposal
is recorded before proceeding. A later guard, preparation or approval can still reject it.
Proposals must not be interpreted as approved plans or completed effects.

The original assistant tool call remains the source fact. Effective arguments are preserved
in inert aicoder.interception.v1 records, with stable call ID/tool/extension/hook correlation.
Delivered executed results mark interception.arguments_changed and the record namespace when
final arguments differ. They do not copy a large/private argument object into model output.
Tools should report the actual affected target; result hooks can annotate safe effective fields
using ToolResultHookInput.call. Inserting credentials into arguments/proposals is inappropriate;
use host credential backends and references. History never reinstalls a callback or route.

```rust,ignore
#[async_trait::async_trait]
impl ToolCallHook for Normalize {
    async fn before_call(&self, input: ToolCallHookInput<'_>,
        _: ExtensionCancellation) -> anyhow::Result<ToolCallEdit> {
        if input.call.name != "write_file" { return Ok(ToolCallEdit::Continue); }
        let mut args = input.call.arguments.clone();
        args["content"] = serde_json::json!("normalized content\n");
        Ok(ToolCallEdit::Arguments(args))
    }
}
```

## Delivered result transformations

ToolResultHookInput exposes the effective call, bounded normalized envelope, immutable
ToolExecutionStatus and optional operation_completed. A successful execution is known complete;
a failure/timeout with no explicit completion receipt stays unknown, not false. Completion is
captured before normalization can replace large error details with an archived preview. Later handlers
see earlier replacements without receiving the earlier raw payload again.

Return None to retain the current view, or ToolResultEdit with is_error, payload and retained
artifact references. Success payload may be any bounded JSON. An error payload is an object
with bounded code/message and optional details. It cannot add unknown error-envelope fields.
A result hook can handle a tool error as a delivered success, while the actual execution status
and completion receipt remain fixed in the interception metadata/journal. ToolExecutionEnded
shows the delivered result; the immutable receipt distinguishes it from raw execution status.
Callbacks returning None preserve the original event category, including TimedOut.

A replacement admits at most 64 KiB and the current tool-output allowance, with at most
256 KiB payload/16 KiB change metadata per chain. Input envelopes admit at most 96 KiB.
These are cooperative boundary limits, not bounds on memory allocated inside native code.
Later replacements may retain only an exact subset of the preceding envelope's eight or fewer
host-normalized ArtifactRef values. They cannot forge a reference, change its MIME/size/
completeness or restore a reference an earlier handler dropped. Only retained top-level
references can create typed media/model previews. JSON resembling an artifact grants no IO.

Each result receipt retains original truncation/capture-failure flags and artifact receipts;
these facts are separate from the replacement's complete delivered payload. Display-only JSON
prefix/truncation hints are cleared for a validated replacement. Compact interception metadata
is charged to the parent output budget; final model/context counting includes the entire view.
Result transformations do not alter auxiliary usage already settled by core.

Normalization can archive original output before result callbacks. Filtering/redaction controls
new delivered content, final tool events and exposed artifact references; it does not erase
source files, earlier events/records or already captured raw artifacts from host storage.
Use tool/backend redaction before capture when raw storage must never contain a value.

A callback timeout, invalid edit, native error or cancellation after an operation produces
explicit incomplete-delivery state with the known/unknown operation completion intact. It
never repeats the operation. Journal failure preserves a persistence failure. Finished records
and delivery_complete describe this interception phase; later global truncation, result
checkpointing, transport or event delivery can still fail and are reported by the owning run.

## Journals and runnable example

RunJournal::record_interception rejects by default. Custom journals must support bounded
Arguments, ResultPending and ResultFinished records before enabling these callbacks. Missing
support for argument edits fails before preparation; missing support for result interception
fails before execution. The explicit unit journal supports nonpersistent SDK runs. Session v3
stores inert Extension entries without shifting logical transcript checkpoint indices; eval
journals retain the same records. No raw pre-transform result text is copied into these receipts.
Old records cannot restore callbacks, grants, storage scopes or model routes on resume.

The offline example uses actual write_file/read_file tools and JSONL sessions. It redirects an
owned write plan before approval, redacts a file read's delivered content, and verifies that
source files, events and model views obey the declared policy. Code Mode uses the same leaves:

```sh
cargo run --locked -p aicoder-runtime --example offline_tool_interception
cargo run --locked -p aicoder-runtime --example offline_tool_interception -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_tool_interception -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_tool_interception -- --code-mode --deny
```

See [native extension contracts](native-extensions.md), [migration](migrations.md),
[acceptance](s7-tool-interception-acceptance.md) and the [remaining Pi audit](pi-parity-audit.md).
