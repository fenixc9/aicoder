# Stdio RPC v1

`aicoder --rpc` is a local, versioned JSONL host adapter over core Agent/RunHandle.
It uses the same runtime settings, resources, tools, session store, budgets and provider
route as the CLI. It is an alternative execution mode; do not combine it with prompts,
stdin/print/JSON/REPL flags or metadata subcommands. `--no-session` keeps multi-turn
history in a private memory repository. No HTTP listener or remote authorization is added.

## Frames and identity

Each stdin line is one UTF-8 object:

```json
{"version":1,"id":1,"command":{"type":"initialize"}}
{"version":1,"id":2,"command":{"type":"prompt","text":"inspect this project"}}
```

IDs are integers from 1 through 9007199254740991, strictly increasing for the entire
connection. Every valid ID is reserved before command validation, including failed
commands. Reusing or decreasing an ID fails without repeating its operation. Keeping a
single high-water mark bounds deduplication memory; clients must reconnect before ID
exhaustion. Invalid JSON/IDs use `id:null`. Unknown fields, commands and protocol versions
are errors. Initialization describes capabilities; it is optional because every request
carries its version. This is aicoder RPC, not JSON-RPC 2.0 or Pi wire compatibility.

Responses are `{"version":1,"type":"response","id":1,"ok":true,"result":{...}}`
or `{"version":1,"type":"response","id":1,"ok":false,"error":{"code":"...","message":"..."}}`.
A prompt response acknowledges ownership with `run_id`; it does not claim task success.
It precedes that run's notifications. Only one run can be owned per connection.
Control input replies can be deferred while [raw hooks](input-hooks.md) run. Correlate replies
by request ID; later replies/events can arrive first. A successful control reply has input_id
or handled metadata. Input IDs need not be consecutive. Status/cancel remain responsive.
An initially consumed submission finishes with status handled and stop kind input_handled;
it creates no model answer or persisted turn. No source/origin field is accepted from clients.

Notifications have no request ID:

- `event`: `version`, `type`, `event` (the unmodified core event-v1 envelope).
- `observation_error`: `version`, `type`, `run_id`, structured core observer `error`.
- `run_finished`: `version`, `type`, `result` with authoritative outcome status,
  run/session identity, stop/persistence failure, usage, budgets, context and delivery data.
  It follows draining the event subscription, even if the stream ended incompletely.
  It does not duplicate raw history or imply that missing events were delivered.

Multiple turns have distinct run IDs. Check each event trace independently; never infer
success from an acknowledgement or a response to cancel/approve.

## Commands

All commands use `type` and reject additional fields. Control `run_id` must match the
currently owned run; old IDs cannot cancel, steer or approve a later run.

| Type | Fields | Effect |
| --- | --- | --- |
| `initialize` | none | Version, commands, limits and execution configuration |
| `prompt` | `text` | Prepare raw input through shared core/runtime and start or consume a submission |
| `status` | none | Selected session, active controls and last bounded completion metadata |
| `steer`, `follow_up` | `run_id`, `text` | Ordered raw hooks/expansion before bounded queueing; may be handled |
| `cancel` | `run_id` | Request cancellation; owner continues to terminal/persistence |
| `approve` | `run_id`, `approval_id`, `decision` | Answer exact pending question; choice only |
| `select_session` | `session_id` (string or null) | Idle only; validate scoped stored session, or select a new session |
| `session_tree` | `session_id`, `limit`, optional `cursor` | Scoped bounded branch-tree page |
| `session_search` | `session_id`, `query`, `limit`, optional `cursor` | Scoped bounded recorded-text search page |
| `permissions` | none | Ephemeral grants for selected session |
| `revoke` | none | Revoke selected/active session grants and expire pending approval |
| `shutdown` | none | Acknowledge, stop accepting input, cancel/await current run and flush output |

`approval_id` is the complete core `{run_id,sequence}` object from a question; `decision`
is `deny`, `allow_once` or `allow_session`. The core checks whether session authorization
is available. RPC never accepts authorization rules, workspace paths, provider routes,
credential values or arbitrary host calls. `--yes` explicitly opts the host into automatic
approval instead of exposing questions. Grant caches live only for this process/host;
session selection clears its previous grants. Recorded history cannot restore authority.

## Bounds, disconnect and errors

Inbound lines and raw prompts are capped at 1 MiB, control messages also obey core's
64 KiB input and 256 KiB aggregate queue budgets after expansion. Output serialization is
capped at 8 MiB per frame; at most 256 frames / 16 MiB await transport. Core event observation
uses its own bounded subscription. No unbounded stdin/stdout worker queue is installed.
Unix transport uses independent nonblocking file descriptions without changing inherited
stdin/stdout flags; it also supports ordinary redirected files.

A slow consumer may lose its event subscription, explicitly reported as observation_error.
Output overflow, oversized frames, write failure or five seconds without write progress
terminate transport with a stderr diagnostic and nonzero exit. The owner is cancelled and
awaited first. No replacement terminal event is fabricated, and no completed operation is
retried. EOF cancels/awaits the owner and drains bounded output; sending shutdown and waiting
for run_finished is preferable to relying on EOF. SIGINT follows the same cleanup and exits
130. Cancellation does not preempt synchronous filesystem/backend syscalls.

Malformed bounded lines receive an error and allow subsequent requests. Oversized/invalid
UTF-8 input is fatal. Per-command errors do not terminate transport; task failure is a
run_finished status. Observation failure is separate from the authoritative task outcome.
Logs and diagnostics go only to stderr. Startup/configuration failures can exit nonzero
before any protocol response. There is no reconnection replay guarantee; reopen
the persisted session through a new explicit host after disconnect. Raw history remains in
the core repository, not in an unbounded RPC completion cache. If an acknowledgement is
lost, inspect the recorded session before submitting another operation; a new connection
does not deduplicate requests from the previous connection.

## Client and verification

The published [JSON Schema](protocol/rpc-v1.schema.json) covers requests, responses and
notifications and references the existing core event schema through a local registry.
[The fixture](protocol/rpc-v1-trace.jsonl) records a real loopback process, including a
denied tool question and completion; workspace paths are anonymized. The development-only
`python3 scripts/check_rpc_schema.py` validates it without fetching remote schemas.

An optional standard-library client demonstrates a complete request/notification exchange:

```sh
cargo build --locked --release -p aicoder-cli
python3 scripts/rpc_client.py 'inspect the project' target/release/aicoder --workspace .
```

The example denies tool questions and prints authoritative result metadata. Configure the
provider through normal runtime settings; Python is only required to run this example.
See [acceptance and limitations](s5-rpc-acceptance.md) for actual tests and measurements.

Native session hooks use an owned asynchronous select_session task. Status reports
session_selection_request while it is pending; cancel_session_selection {request_id} cancels only
that owned request. Shutdown/EOF also cancel and await it. Native selection resolves a new session
to its actual ID and may return session_lifecycle observations; these are inert data, not rules or
restored authority. Completed adoption survives later observation cancellation. See
[session lifecycle](session-lifecycle.md).

`fork_session {point}` starts a bounded owned fork job for the selected session. Points use
`{position:"current"|"root"|"before_last_user"}` or `{position:"before"|"at",entry:"ID_OR_LABEL"}`.
It shares the session-selection owner slot, status and targeted cancellation. A completed reply
returns the new session ID and inert fork provenance/editor data; editor text is never submitted
automatically. See [session fork](session-fork.md).
