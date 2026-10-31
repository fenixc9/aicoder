# Host UI capabilities (S6)

UI is ephemeral host interaction, separate from model events, canonical session history and
tool approval. Core defines bounded data and a host/client broker; it imports no terminal,
HTTP or browser types. Hosts advertise notification, selection, text input and status
capabilities and expose immutable extension commands through their own command surface.

Each run/command owns a fresh UI scope. A client is a weak, cancellation-scoped capability;
retaining one cannot keep a host alive. Questions have opaque scope/sequence IDs, deadlines
and typed answers. Invalid, stale, duplicate or foreign answers fail without consuming a
valid question. Cancellation, timeout, dropped question futures and scope closure expire
pending requests. Closing an observer does not cancel an execution owner; closing the host
expires its UI and the host separately cancels/awaits execution through existing controls.

Notifications and statuses are bounded retained display state, not acknowledged delivery.
Questions, retained notices, status keys, serialized payloads and accepted scope operations
have explicit limits. Full admission fails; old notifications may be evicted with an explicit
omission count. Cancellation/drop cleanup is synchronous and needs no spawned runtime task.

A missing UI returns Unsupported immediately, without opening stdin or guessing answers.
Extensions can inspect capabilities and provide explicit headless behavior. Selection/input
answers carry data only; no UI answer can approve tools, change provider routing or restore
authority. Commands use existing ExtensionSet registration/dispatch. A returned Submit goes
through the host's normal AgentInput path; UI metadata alone never starts a model or tool.

TUI renders plain text, bounded choices, an independent input editor and status/notice views.
Web uses authenticated run-scoped snapshots/answer routes and inert DOM text. Both preserve
approval priority and reject late replies after scope/owner changes. Frontend capability
advertisements and command catalogs are host-selected; client payloads cannot enable them.

## SDK ownership

A generic host creates `UiHost::new(capabilities, limits)` (or `interactive()`), attaches its
`client("host")` through `TurnExecutionContext::with_ui`, and retains the host for one owned
operation. `snapshot()` returns bounded display state; `wait(revision)` waits for changes
without losing a notification and also observes deadlines/cancellation. `answer(id, answer)`
validates against the original immutable prompt. Hosts create fresh scopes and close them
when an operation ends. A client cannot deserialize or construct a live scope from history.

Tools use `ToolContext::ui()`. Context/tool hooks and commands use `ExtensionCancellation::ui()`.
Derived clients retain every inherited cancellation boundary (up to MAX_UI_SCOPE_DEPTH = 64);
rebinding to a fresh context cannot revive an expired client, and excess depth fails closed.
Callback children inherit cancellation and expire when their callback ends; tool clients expire
when dispatch ends, including failures/steering. Retaining a client does not retain run controls,
approval sessions or authority. The core InputContextProvider trait gains an additive default
`prepare_for_execution` method so execution-scoped services reach context preflight, model
selection, ordinary turns and manual summaries without changing older providers.
Runtime::summary_agent_with_extensions preserves the explicit native snapshot when constructing
a separate manual-summary host; the TUI uses it for `/compact`.

```rust,ignore
let host = aicoder_core::ui::UiHost::interactive();
let context = aicoder_core::TurnExecutionContext::new()
    .with_ui(host.client("application")?);
// Pass context to the existing Agent API. The frontend drives host.snapshot()/answer().
// On shutdown: cancel/await the execution owner, then close/drop the UI host.
host.close();
```

SDK/CLI/RPC hosts without an attached UI get Unsupported immediately. There is no implicit
terminal access, stdin reader, default selection or automatic dismissal-as-approval. A native
extension can explicitly return a headless notice or otherwise handle Unsupported. UI activity
has its own admission/deadline limits; callbacks/tool execution retain their existing outer
budgets. A command callback has its declared timeout (at most 600 seconds for human interaction);
context/tool interception hooks retain their 30-second ceiling. UI
questions never cause model usage; a returned AgentInput uses normal model preflight, budgets,
usage, approvals and persistence. Collect data before preparing a mutation; a later answer
cannot change the owned plan that the user approved.

Core's broker contains no rendering, HTTP, terminal, filesystem or process-launch code. Native
Rust callbacks use these host services. Existing process-tools/MCP wire capabilities remain
unchanged; they do not implicitly acquire a new server-initiated UI protocol. A host can use
native hooks around those same registered tools. Installing a resource package does not
start code or attach UI capabilities from manifest/history metadata.

## TUI adapter

The `aicoder-tui` crate now has a reusable library entry point:
`run_with_provider(runtime, provider, extensions)`. The existing binary remains a wrapper over
the same frontend. Embedders retain optional executable activation owners and await their
cleanup after the frontend returns. Normal exit cancels/awaits the operation; dropping its
future signals cancellation and expires UI, while cleanup can continue on the caller's live
runtime. No async API can await from a synchronous Drop.

`/ext` lists frozen command metadata; `/ext NAME JSON` invokes a registered command while idle.
The namespace preserves built-in command ownership. Commands cannot navigate or replace an
active run. An existing session is opened under its workspace/run lease during command
execution. A Notice becomes transient timeline text; a Submit goes through the same AgentInput
submission function as a normal prompt. Cancellation before completion delivery suppresses
submission. Resource reload preserves the host-selected extension set and rotates normal
registry/approval state through the existing replacement path.

Selections use Up/Down and Enter. Text uses the independent bounded Unicode editor, cursor
navigation, explicit Enter submission and Alt+Enter for multiline input. Escape dismisses;
Ctrl-C cancels the owner. Recognized paste is input data and never selects, submits or approves.
Approval rendering/keys have priority over extension questions. Field drafts do not replace the
normal composer; notices are transient bounded timeline previews, and statuses are displayed
in the status area. Existing scope status keys clear on close.

Run the temporary scripted example (no model service/credentials):

```sh
cargo run --locked -p aicoder-tui --example offline_extension_ui
# In the terminal: /ext review
```

## Web adapter

`WebOptions::extensions(Arc<ExtensionSet>)` explicitly installs native registrations while
constructing the host. Browser payloads cannot install extensions or choose workspace/provider
routing. Existing authentication, host/origin checks, body/response/concurrency limits cover
the new routes. Request IDs share the existing strictly increasing JavaScript-safe admission
sequence. One command job and one model run are retained; active work is mutually exclusive.

| Route | Meaning |
| --- | --- |
| GET /api/commands | Frozen command specifications |
| POST /api/commands | Start an owned command with id/name/arguments |
| GET /api/commands/{id} | Command UI and terminal result |
| POST /api/commands/{id}/ui | Typed answer for that job's live question |
| POST /api/commands/{id}/cancel | Cancel a still-active command |
| POST /api/runs/{id}/ui | Typed answer for that model run's live question |

Run snapshots add `request_id` and `ui`; `/api/state` also reports the current command job.
UI snapshots are version 1 and expose scope, revision, capability flags, questions, retained
notices/omission count and statuses. An answer body has `question_id: {scope, sequence}` and
`answer: {kind: "choice", id}` / `{kind: "text", text}` / `{kind: "dismissed"}`. Unknown shapes
fail JSON extraction; invalid choices/text preserve the pending question. Stale, duplicate,
foreign or expired questions fail with conflict. None of these fields accepts approval rules.
The existing `/approval` route still accepts its separate run-bound approval ID and decision.

Command callbacks run outside request handlers, so browser polling/disconnection does not own
execution. A Submit is accepted once under the same host-state lock as cancellation/admission;
it launches a normal AgentInput turn after the command's session lease is released. Command
completion and model completion remain distinct. Shutdown closes admission, cancels both
owners and awaits them. Browser controls render with inert DOM text, preserve field drafts
across polling, show UTF-8 byte budgets and separate extension questions from approvals.
Notifications are displayed from retained scopes even when a fast command immediately submits.
SSE delivery remains a single observation without replay; UI snapshots are separate from the
canonical event trace and do not become historical facts.

A temporary offline Web example uses a caller-supplied ephemeral AICODER_WEB_TOKEN and prints
only its loopback URL, never the token:

```sh
cargo run --locked -p aicoder-web --features server --example offline_web_extension_ui
```

## Bounds

Default/hard ceilings: 4/16 pending questions, 32/64 retained notices, 32/64 status keys and
4,096/65,536 accepted operations per scope. The default question timeout is 120 seconds;
host overrides range from 1 ms to 600 seconds, and outer callback/run limits can expire first.
Prompt serialization is at most 16 KiB; titles are 512 UTF-8 bytes, choices at most 32 with
64-byte IDs and 256-byte labels, and text fields accept at most 16 KiB. Both serialized and
field ceilings apply. Notices are 2 KiB, status text 1 KiB; control characters except LF/tab
are rejected. Input max_bytes measures UTF-8, independently of model tokens/provider framing.

Accepted operations share a bounded counter; malformed/full requests do not consume questions.
Responses and cleanup remain available after admission exhaustion. Snapshots/response objects
own their data; changing a frontend copy cannot change the stored question. Cancellation,
expiry, Drop and close remove pending senders synchronously. Scope close clears status keys;
retained notices remain inspectable until their host is dropped/replaced. No UI data restores
live backend handles, process plans, tool grants or credentials.
