# Boundary context drafts and model work

Successful-round and settlement callbacks can stage custom messages, context edits and native
compactions, in addition to existing data/context entries. They use the same Agent loop and
journal as tools, controls, semantic summaries and frontend hosts. Nothing is installed by default.

`BoundaryEntry::CustomMessage { kind, content, display, details }` records a sourced Custom message.
The registered extension supplies its source; the hook stays in the receipt provenance. Text and
images use the ordinary custom-input validation and capability checks. Details remain inert, and
display controls presentation rather than model inclusion. Fresh-message finalizers run once before
publication; source labels, metadata and hidden display survive canonical storage and events.

`ContextEdit { target_id, replacement }` appends the ordinary branch-local edit record. Targets are
exact canonical IDs, including an earlier staged message ID. Roles, calls/provider state, tool result
identity/status and media references retain the [context-edit rules](context-edits.md). Omitting a
result removes its complete call unit. The original message remains canonical.

`Compaction { summary, first_kept_entry_id, details }` appends a host-authored reduction. Some(ID)
retains the complete raw suffix beginning at a non-Tool message; None keeps the new summary and
subsequent messages, including later entries in the same batch and future turns. The latter can
produce an archive window with zero materialized raw messages, which still resumes normally.
Counts describe original records, including custom/native context, rather than projected message
count. The most recent native or semantic reduction selects the model view on its branch. Earlier
records and reductions remain available. Details never become model instructions. Interrupted,
recovered and omitted archive-window uncertainty stays explicit.

Callbacks see three separate inputs. `history` contains acknowledged canonical messages; `context`
is the existing core model context. `context_preview` includes earlier staged proposals. The optional
`session_context` supplies the candidate persistent projection and stable record IDs; it may include
retained records absent from a transiently pruned core context. Its metadata is provisional data,
not a repository cursor or authorization. Entries discarded by replacement do not become history.

Each message/edit/reduction proposal receives an owned ID before later callbacks see its preview.
Finalization and final preparation retain those IDs. The final plan binds the current session
revision/leaf, exact transaction and projected model view. Memory and JSONL preparation is read-only;
JSONL validates its opaque native cursor and deep-validates edits against real targets. Cold edit
payloads cannot use private descriptor content. A retained cold suffix outside the admitted window
requires a larger host load budget. The complete view preserves the runtime prefix and passes
adapter/model/counting policy before publication. Steering, cancellation, admission failure or a
source conflict publishes no partial draft batch. Each acknowledged batch containing reductions
emits one `native_boundary_batch` compaction start/completion pair and increments the context
compaction count once; all constituent records remain in history. A failed acknowledgement emits
failure instead of claiming completion, keeping event traces and terminal counters consistent. An acknowledged batch is adopted before a later
budget/cancellation check. Completed model/tool effects are never implicitly repeated.

`RunJournal::boundary_context`, `prepare_boundary` and `commit_prepared_boundary` are optional
extension points. `PreparedBoundary` and `SessionAppendPlan` are one-use native objects, without
serialization or restored authority. A session journal binds plans to its private owner. Basic
journals and `()` can append messages and perform self-retaining compaction; ID-bound edits and
retained-tail compaction require session support. EvaluationJournal captures scoped model work and
self-retaining drafts directly, so benchmarks do not parse terminal output. Startup keeps its
separate data/context-only contract; these drafts run at live paired boundaries.

## Scoped model work

A callback may use `input.models.generate(provider, request)` with an explicit immutable host
provider object. It never selects a credential route from history or proposal data. The provider
must declare a known window, and the request must offer no tools, select ToolChoice::None and
reserve 1..8192 output tokens within declared model capabilities. Shared token counting admits
input plus output reservation to the parent token/call/concurrency budgets before provider IO.
Requests are bounded to 2 MiB and run without streaming UI output. Response delivery is bounded to
256 KiB and rejects actions/images. This is a text-model facility, separate from image-generation
plans and scoped raw artifacts.

Started/Finished auxiliary receipts describe the model and counting scope, without private prompt
or response text. Known usage is captured before response validation and receipt work. Every
attempt is charged even when its draft is replaced, parsing fails or the callback is cancelled.
Unknown usage uses the reservation as estimated accounting; it cannot become free billed work.
Primary and auxiliary usage stay separate. Pending model futures expire with their callback and
cancel their children on drop, without waiting for runtime shutdown. A failed delivery/receipt
never retries a completed request. All budgets and elapsed preparation remain in the existing run.
Use this managed facility for boundary model work; arbitrary native host code remains trusted host
code, as in the [extension contract](native-extensions.md).

Finished auxiliary receipts without terminal turn statistics remain known durable usage. Complete
terminal statistics own their turn's total, preventing double accounting. Started-only or partial
requests remain uncertain. These observations do not prove exact billed cost.

Native reductions use `aicoder.context.native-compaction.v1`, version 1. Older binaries may ignore
this namespace; upgrade before continuing a session that depends on its reduced view. Existing
semantic v1/v2 checkpoints keep their rules. Restoration loads data and original branches, never
callbacks, routes, grants, backend handles or continuation authority. See [migration](migrations.md).

Run the executable [file-tool example](../crates/aicoder-runtime/examples/offline_boundary_drafts.rs):

```sh
cargo run --locked -p aicoder-runtime --example offline_boundary_drafts
cargo run --locked -p aicoder-runtime --example offline_boundary_drafts -- --window
cargo run --locked -p aicoder-runtime --example offline_boundary_drafts -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_boundary_drafts -- --code-mode --window
```

It uses a real owned file mutation, approvals, optional Code Mode, JSONL and a scripted auxiliary
provider. Approval denial starts no auxiliary request. Allowed variants preserve original tool
receipts, apply the draft before the next main request and reopen the final archive view. These
are offline contracts, not real-model coding scores. [Acceptance](s7-boundary-drafts-acceptance.md)
records local checks. [Compaction interception](compaction-hooks.md) covers automatic/manual
before/after callbacks separately. Tree summaries and final isolated Spark/Pi/SWE quality/release
acceptance remain open.
