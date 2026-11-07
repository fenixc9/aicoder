# Persistent context edits

Native hosts can append a branch-local replacement or omission of an existing message in the
model view. Canonical messages, tool receipts and raw archive pages remain intact. This differs
from fresh-message finalization, which decides the message actually recorded, and request hooks,
which change an ephemeral request without durable edits.

```rust,ignore
use aicoder_core::{session::*, TurnExecutionContext};
let outcome = agent.navigate_session(
    repository,
    session_id,
    ArchiveNavigation::EditContext {
        edit: ContextEditRecord {
            version: 1,
            extension: "my-extension".into(),
            hook: "reduce-output".into(),
            target_id: entry_id.into(),
            replacement: ContextReplacement::Text("concise retained content".into()),
        },
    },
    TurnExecutionContext::new(),
).await?;
```

Use the Agent path for model/counting preflight and native session guards. It resolves a target
ID or unambiguous label under the workspace/session lease, freezes the append and preview,
validates provider/model compatibility, and commits the same plan. A denied, cancelled, stale,
invalid or oversized proposal writes no edit. Post-commit observation failure returns the adopted
session plus delivery diagnostics; it cannot roll back or repeat a completed append. Session grants
expire through the existing adoption path. Stored extension/hook strings express provenance only.
Lower-level SessionNavigator preparation/execute provides repository validation and ownership;
it does not substitute for the Agent's model or lifecycle checks.

The versioned `aicoder.context.edit.v1` extension record targets a canonical message on its own
ancestry. It cannot edit runtime or sourced System instructions, receipts or other metadata.
The latest record for each target wins on the selected branch. Checkout before that edit restores
its older projection. `Restore` explicitly cancels the target's earlier replacement or omission;
other omissions affecting the same tool unit still apply. Raw search, exports and forks retain the
original records and edit records separately. `ContextOrigin::Edited` identifies both source IDs.
Raw recorded-message/accounting counts do not become counts of the reduced view.

`Text` replaces user content; custom messages retain their source label. Assistant replacements
retain original tool calls and opaque provider state. Tool text edits retain result IDs and error
status, replace output/error text and omit image previews. `Blocks` supports typed content while
preserving role, custom provenance, assistant calls/provider state and tool-result IDs/error flags.
Image sources must already occur in the original message; edits cannot create URL/artifact IO.
Malformed scalar tool payloads are handled without indexing panics. Each serialized edit and each
nontrivial replacement message is bounded to 64 KiB. Large protected state may prevent a text edit.

Omitting an assistant call or a result removes the entire projected call/result unit. Multi-result
records connect all their calls. Pairing follows actual ordered projection provenance, so reusing
a call ID in another turn cannot omit the unrelated turn. Missing-result recovery follows its
owning call; retained interruption/summary uncertainty stays visible. Typed replacements of a
multi-result raw record are validated whole, then split by the original projected result identity.

JSONL windows keep edit payloads out of their private metadata index. They load only the latest
real edit for each materialized target, charge those complete records to the window byte budget,
and preserve original archive counts/cursors. Cold edit targets are read and validated before
publication even when they do not appear in the current bounded view. An admission failure does
not publish a partial edit. Window continuation and reopened views use actual immutable payloads.

Existing summary checkpoints retain their original raw anchors. Editing a message already reduced
into a summary does not recreate it; editing/omitting its retained boundary does not invalidate the
checkpoint. [Projection-aware semantic summaries](summary-projection.md) now map retained boundaries
through canonical IDs, so editing the newest retained unit does not require a fresh raw suffix.
Custom window backends/journals without lineage support keep the old safe suffix path.
[Live boundary drafts](boundary-drafts.md) now stage persistent edits and compactions atomically;
before-compaction replacement and tree summaries remain open.
This API is currently native SDK functionality, without a new stock frontend edit command or endpoint.

Run the executable examples without credentials or network:

```sh
cargo run --locked -p aicoder-runtime --example offline_context_edits
cargo run --locked -p aicoder-runtime --example offline_context_edits -- --window
```

Both use the normal Agent loop with a scripted provider, receive the replacement in one request,
and verify that the original target remains canonical. See the [acceptance](s7-context-edits-acceptance.md).
