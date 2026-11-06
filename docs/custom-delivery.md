# Custom messages during an owned run

RunHandle::begin_custom reserves order/capacity synchronously and returns an owned future.
submit_custom is the async convenience. Both accept CustomMessage and CustomDelivery:

| Delivery | Commit / visibility | Continuation |
| --- | --- | --- |
| Steer | Existing steering boundary, after paired tool results | One next request in the same run |
| FollowUp | After accepted completion | One next request in the same run |
| TurnEnd | Safe round boundary; rechecked after verification/settlement waits | No forced request |
| NextTurn | Bounded in-memory session inbox; committed with a later explicit trigger | Never starts a run |

Existing steering semantics remain: pending preparation/approval/dispatch can be superseded;
already executing operations finish according to their backend cancellation contract and are
never repeated. Canonical messages retain Custom role, registered source, display intent and
inert details. Model views use the normal compatible projection. These are host data APIs;
they do not grant tools, routes, credentials or session approvals.

Custom submissions share the ordered reservation lane with raw user input, but bypass raw-input
hooks and template expansion. Dropping unpolled or waiting futures releases capacity. Late
submissions reject after closure. RunControlConfig bounds input size and aggregate pending data;
custom admission additionally caps a run at 256 messages / 8 MiB. Submission waiting has a
30-second ceiling and observes remaining parent duration. Rejections retain the original custom
envelope. User begin_input cannot use the new Context-only RunInputKind.

RunInput adds optional custom metadata alongside its single canonical message. Old user wire
values omit it. Role/metadata consistency and source labels are validated before application;
custom content and current model capabilities/window policy are checked before journal writes.
RunJournal::supports_custom_input defaults false; input_custom atomically stores delivery metadata
and the typed custom entry. Built-in session/evaluation journals and () support them. Unsupported
journals reject prequeued customs before provider IO and reject later custom admissions. A Queued
acknowledgement is admission, not a claim that history was written or a model saw the message.

Deferred context cannot split a tool-call/result unit. A final candidate is checkpointed first,
so recorded order matches canonical order even if context becomes the final transcript entry.
The final answer stays the actual assistant response. Context received while completion verification
waits is flushed before an immediate retry; settlement rechecks accepted arrivals before closing.
A failed/cancelled boundary retains unapplied controls in the outcome. Completed model/tool work
and paid usage are retained across persistence failures; no operation is retried to capture data.

## Next-turn host inbox

Agent owns a CustomInbox bound to workspace and session IDs. NextTurn requires an Agent-bound
inbox and selected session; stateless/unbound callers receive SessionUnavailable. The inbox holds
at most 32 messages / 4 MiB across sessions. pending and discard expose explicit host management;
discard rejects CustomBusy while a preparation owns a claim. Neither inboxes nor claims serialize.

After a fresh user/custom trigger survives input preparation, Agent claims that session's queued
messages. Model/request/startup validation sees them, but the trigger identity remains the fresh
submission. The initial transaction records the trigger, queued customs, safe origin receipts and
startup context together. Only a successful transaction acknowledgement consumes the claim.
Failure, cancellation or dropped preparation releases it. New arrivals during a claim remain for
the following run. Handled user input does not claim or consume pending context.

Accepted next-turn messages survive cancellation of the sending run within that host. They do
not appear in its model view or serialized history before consumption. A newly constructed Agent
starts with an empty inbox. custom_inbox / with_custom_inbox explicitly transfer bounded host data
between compatible generations; workspace mismatch rejects. Reasoning clones and TUI resource /
settings reload retain the selected inbox. Stored receipts never reconstruct queues or callbacks,
restore grants or automatically launch a model. Host shutdown discards unconsumed memory data.

Delivery records use aicoder.custom-delivery.v1. Next-turn consumption uses
 aicoder.custom-next-turn.v1 with queue ID and originating run/input IDs, alongside the real typed
custom entry. These are inert provenance; canonical custom content stays separate from projections.
V1 event schema adds Context and optional custom metadata with role pairing. EventTraceValidator
checks run identity and typed envelopes. TUI displays custom controls as extension data, respects
hidden intent, and creates no User row.

Run the [offline runtime example](../crates/aicoder-runtime/examples/offline_custom_delivery.rs):

```sh
cargo run --locked -p aicoder-runtime --example offline_custom_delivery -- --delivery turn_end
cargo run --locked -p aicoder-runtime --example offline_custom_delivery -- --delivery follow_up
cargo run --locked -p aicoder-runtime --example offline_custom_delivery -- --delivery next_turn
cargo run --locked -p aicoder-runtime --features code-mode --example offline_custom_delivery -- --code-mode --deny
```

It uses actual owned file tools, approvals, JSONL, bounded archives and inert export, with a
scripted provider and no external model. [Acceptance](s7-custom-delivery-acceptance.md) records
normal/Code Mode variants. [Migration](migrations.md) covers source and schema changes. Message
transforms, session lifecycle/navigation veto and boundary context-edit/compaction drafts remain
in the [Pi audit](pi-parity-audit.md), along with final real-model comparison/release acceptance.
