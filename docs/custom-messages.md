# Sourced custom messages and idle runs

`CustomMessage` is explicit host/extension content, separate from user input and inert metadata.
It carries a source, custom type, text/images, display intent and optional details. `AgentPrompt`
is User(AgentInput) or Custom(CustomMessage). Existing Agent user entry points keep their input
contracts; new run_prompt / run_prompt_outcome, run_prompt_controlled and session prompt variants
accept either trigger through the same executor.

Use `CustomMessage::text(source, custom_type, text)` or new(source, custom_type, content).
with_display(false) suppresses the standard TUI row while retaining model context. with_details
adds inert persisted JSON, never model instructions or restored authority. Details are public
session data; the model receives only the sourced message content. Text/image envelopes use the
same validated URL/inline-image rules as explicit input: at most four images, no tool-artifact
references, credential-bearing URLs, reasoning, tool calls/results or provider-private blocks.
Empty content is allowed and still has its source/type label. Details are bounded to 32 KiB;
the complete custom envelope is bounded to 2 MiB minus storage headroom.

## Canonical history and provider projection

Custom messages have canonical MessageRole::Custom and a source label. They are never stored
as ordinary User messages or confused with a user's submitted intent. The core projects them
to a labelled, provider-compatible User-role message before capabilities, counting/window
admission and HTTP encoding. Images remain explicit bounded input; core fetches/decodes no
URL or pixels. CustomMessage::model_message provides that compatibility view for explicit SDK
clients; keep message() and typed entries as canonical records. Unprojected Custom roles reject
at provider validation/encoding instead of leaking an unsupported role.

`SessionEntryData::CustomMessage { custom }` uses the existing Extension wire shape under
`aicoder.custom-message.v1`, version 1. New readers validate and materialize it; old readers
retain inert extension data. Ordinary Message entries cannot contain System or Custom roles,
and generic Extension values cannot impersonate reserved typed messages. Message views/search,
branches, summaries, windows and accounting use real recorded entry IDs. Typed details stay out
of model views and text summaries. Summary input retains custom facts and replaces image bytes
with explicit omission data, preserving raw history.

The declared runtime prefix is independent of leading recorded extension context. Prefix
replacement preserves native context at a branch root, and a host prefix that happens to use
a native-looking label remains host-owned. Private archive descriptors omit bodies/details;
only bounded materialized records enter context. TUI respects display intent; explicit JSON /
inert HTML exports retain canonical source and details. Display is not model exclusion or a
privacy flag.

## Store without a model or start one idle run

`Agent::append_custom_message(repository, id, custom, context)` acquires the existing idle
workspace/session lease, prepares a read-only model-selection view, validates compatibility /
budgets, and appends one custom entry. It invokes neither raw-input/expansion nor startup hooks
and runs no model/tool or TurnStart/TurnEnd. Recover unfinished turns before this idle mutation.
Request-view preview hooks retain their documented idempotent preparation semantics.

`run_prompt_with_session_outcome(..., custom, ...)` atomically commits the initial custom message,
turn metadata and any startup context before provider IO. It shares normal model selection,
reasoning, approvals, cancellation, controls, context policy, summaries, output limits and usage.
A failed/cancelled preparation leaves the initial transaction unwritten. A trigger is a fresh
message: appending it first and then triggering the same payload intentionally records two entries.
Existing control leases and registry binding are checked before committing a trigger.

Custom triggers skip raw user input hooks and skill/template expansion. A text beginning with
`/` stays literal custom content. `BeforeStartInput::trigger` is User or Custom with full custom
content/metadata; low-level execution without an explicit trigger leaves it None. Startup still
runs once per run. Recorded details/prefixes/tool choices never reinstall callbacks, routes,
approval grants or an automatic run after restoration.

## Native command hosts

`CommandOutput::SubmitCustom(custom)` explicitly requests a custom-triggered run.
ExtensionRegistry validates the entire envelope and binds the source to the registered command
owner before returning it. `CommandOutput::into_prompt` normalizes user/custom submissions.
TUI and authenticated Web command hosts use the same Agent session path after command completion,
with existing cancellation/supersession checks and host bounds. They add no synthetic User row
or transcript message. Notice remains a no-run command result; Submit remains user input.

[Busy custom delivery](custom-delivery.md) adds steer/follow-up, deferred no-trigger context and
session-scoped next-turn inboxes. Message transforms, session lifecycle veto and boundary
context-edit/compaction drafts remain in the [Pi audit](pi-parity-audit.md).

Run the [offline runtime example](../crates/aicoder-runtime/examples/offline_custom_messages.rs):

```sh
cargo run --locked -p aicoder-runtime --example offline_custom_messages
cargo run --locked -p aicoder-runtime --example offline_custom_messages -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_custom_messages -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_custom_messages -- --code-mode --deny
cargo run --locked -p aicoder-runtime --example offline_custom_messages -- --record-only
```

The fixture uses actual tools/approvals, JSONL, archive windows and inert export without an
external model. [Acceptance](s7-custom-messages-acceptance.md) also records JSON/SSE loopback
projection, authenticated custom commands and real TUI delivery. See [migration](migrations.md).
