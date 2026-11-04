# Actionable round and settlement boundaries

`Extension::boundary_hook(HookSpec, Arc<dyn BoundaryHook>)` installs an explicit immutable
host callback in the existing executor. Nothing is installed by default. It uses the same
ordering, scoped cancellation and sanitized failure contract as [native extensions](native-extensions.md).
The callbacks are mutation stages, separate from notification observers.

`TurnEnd` runs after a successful model round and its paired tool results. For a final answer
it runs before completion verification. `BeforeSettle` runs only after the completion verifier
accepts a candidate and queued controls have been checked. Neither stage runs as an alternative
completion path for a failed/cancelled model or a rejected candidate. Tool rounds still require
the ordinary next model request even if a callback clears the continuation flag.

`BoundaryInput` borrows canonical history, the current model context, accumulated proposals and
a statistics snapshot. The latter separates primary and auxiliary usage while including every
admitted attempt in total accounting. `can_continue` describes the current round/token capacity;
it does not reserve future capacity or promise that a later duration/output check will pass.

A returned `BoundaryAction` can replace the staged entry list, append entries and set/clear the
continuation flag. Lower priorities run first; ties retain installation order. Later callbacks
see earlier proposals and the current flag. Replacements take the replacing callback's provenance;
appends preserve existing provenance. Callback results cannot edit recorded history, routing,
approval rules, the verifier result or the terminal answer directly.

Two entry kinds are supported:

- `Data { key, value }` is inert extension metadata. It is never inserted into a model prompt.
- `Context { kind, text }` creates an explicitly sourced native context message with a validated
  extension/hook/kind label. It is a system/context message, never synthetic user input. It is
  recorded even without continuation; the current answer remains the model's actual answer.

A chain admits at most 32 entries, 32 KiB per entry body, 64 KiB for the complete staged list,
256 KiB across successive staged lists and 128 KiB for its final receipt. The complete receipt
and projected messages charge the parent output budget before publication. Excess data rejects
the action as a whole. Ordinary next-request token/window admission still applies. Each HookSpec
has its timeout, and the complete phase and journal calls have a 30-second ceiling. Cancellation
or steering drops uncommitted proposals and expires callback children. Callback errors/panics
follow the sanitized native-hook failure policy; completed model/tool work is never retried.

Before the first provider request, `RunJournal::record_lifecycle` must accept a Started record.
`commit_boundary` must durably commit the Applied receipt and all derived context messages as
one batch. The built-in session journal and evaluation journal support this; custom journals
default to rejection, so unsupported persistence fails before paid model IO. `()` explicitly
supports ephemeral runs. Acknowledged messages enter canonical history before a post-commit
budget check, preserving facts when the commit consumes the remaining time.

Continuation stays in the same executor/run and shares all round, token, tool, elapsed and
output budgets. Follow-up/steering queues remain open while settlement waits and are rechecked
before returning. Steering discards pending proposals; follow-up is applied through the existing
input journal/admission path. A continuation request at exhausted round/token capacity returns
an explicit budget stop and commits no proposed entries.

## Session and frontend representation

`SessionEntryData::NativeContext { source, message }` is the typed domain form. Its JSONL wire
form remains `kind: "extension"`, namespace `aicoder.context-message.v1`, with version 1 source
and message data. Existing readers retain an inert extension record; upgraded readers validate
and materialize it. The reserved namespace cannot be constructed as generic Extension metadata.
Ordinary `Message` entries still prohibit the System role. See [migration](migrations.md).

`as_message()` and session projections include native context as real recorded messages with
actual entry IDs. Branches, bounded archive windows, semantic summaries, TUI history and inert
HTML export preserve the source. Private archive descriptors never become prompt text.
Restoration retains messages and inert receipts, without reinstalling callbacks, grants,
provider routes or automatic continuation. The metadata-only `lifecycle_boundary_committed`
event reports kind/counts/continuation; it carries no context/data bodies.

Run the [offline runtime example](../crates/aicoder-runtime/examples/offline_boundary_hooks.rs):

```sh
cargo run --locked -p aicoder-runtime --example offline_boundary_hooks
cargo run --locked -p aicoder-runtime --example offline_boundary_hooks -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_boundary_hooks -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_boundary_hooks -- --code-mode --deny
```

It uses actual owned file tools, approvals, JSONL, archive windows and export, with a scripted
provider and no credentials/network. [Acceptance](s7-boundary-hooks-acceptance.md) records the
measured checks. This implements data/context/continuation boundary actions; Pi's context-edit
and compaction drafts, input/start/message transforms and session navigation veto remain
separate work in the [audit](pi-parity-audit.md).
