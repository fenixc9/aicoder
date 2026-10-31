# Process tool protocol v1 (S6)

The optional runtime process-extensions feature hosts explicitly selected programs as
dynamic tools. Core owns serializable tool-contract types; runtime owns concrete stdio,
process lifetime and tool adapters. Default core/runtime/frontends do not start a server,
load a project-selected executable or inherit credentials. Package discovery/installation
and MCP negotiation remain separate roadmap items.

## Wire contract

Frames are UTF-8 JSON objects terminated by a newline. Envelopes follow
[JSON-RPC 2.0](https://www.jsonrpc.org/specification); the host uses positive, non-reused
integer IDs and one active request per connection. This profile does not send batches or
permit unsolicited server frames. Logs belong on stderr. The initialize request selects
protocolVersion = aicoder.tools.v1; its result must echo that exact version and return
server identity and the complete bounded tool catalog. Unknown versions fail initialization.

Methods are initialize, tools/prepare, tools/call, tools/commit and tools/dispose.
Serializable request/result contracts live in core extensions::process_protocol. The
[published JSON Schema](protocol/process-tools-v1.schema.json) and [synthetic wire examples](protocol/process-tools-v1.jsonl)
are checked against those public Rust types; they are transport examples, not session data
or reusable plan authority. A tool
declares whether it supports preparation; descriptors never supply host approval rules.
Host aliases and capability overrides are explicit. All tools default to Mutating even
when a server describes them as read-only; only host-selected overrides change that.

For prepared tools, prepare reads/captures an immutable operation and returns an opaque
token plus bounded preview. It must not change the workspace or create persistent staging
files. The host retains that exact connection/tool/token in an owned PreparedTool, presents
the preview through normal core approval, and commits the token once after approval.
Commit must consume the captured operation, preserve backend conflict checks and never
re-plan from changed arguments/files. Dispose releases the token idempotently; denial/drop
queues disposal, and an unconfirmable disposal closes the process to release its memory.
Tokens are not serialized into events, sessions, snapshots or tool results.

Unprepared tools are called only through execute, after normal host approval. Successful
results contain JSON output and a truncation flag; peers cannot mint host artifact references
or restore session authority. Core retains normal hook, approval, accounting, artifact and
journal behavior. Loading an extension is execution of trusted host-selected code, not a
sandbox or a guarantee that a dishonest implementation honors preparation/read-only claims.

## Ownership and bounds

Launch configuration fixes an absolute executable, argv, canonical workspace and explicit
environment. Environment inheritance is disabled. Debug/errors do not expose argument or
environment values, plan tokens or raw stderr. Server work runs with the user's permissions.
The host must trust the executable and its package/working-directory behavior.

A bounded OS worker owns nonblocking pipes and an unreaped process-group leader. Caller
cancellation/drop can signal that owned group synchronously; reaping remains independent
of Tokio shutdown. Queued cancellation skips unsent work; active cancellation/timeout
poisons the connection and kills its process group. Explicit shutdown awaits the worker's
bounded cleanup report. Cleanup that cannot confirm reaping is reported as incomplete,
never promoted to success. Same-group descendants are terminated as well; detached groups
cannot be contained by this mechanism.

Frames, queued work, outstanding plans, schemas, initialization/request deadlines and stderr
scans are bounded. Both streams are drained while retaining bounded state. Malformed,
wrong-ID, duplicate, oversized or unexpected frames poison the connection. A final complete
reply remains a completed operation even if the peer exits afterward. Crashes and partial
replies identify whether execution may have occurred; no request is retried or restarted
implicitly. may_have_executed describes a tool call/commit, not a promise that trusted
server startup code never ran. A new connection requires a new registry snapshot/authority.

These are admission and cooperative/process-cleanup bounds, not a process-memory sandbox
or a way to preempt a stuck OS spawn/filesystem syscall. Servers may access the host beyond
their working directory. Starting a trusted server and tool approval are separate authority
decisions; model input never supplies the executable, environment or launch configuration.

## Default admission limits and host example

| Resource | Default | Hard configurable maximum |
| --- | --- | --- |
| Request including newline | 256 KiB | 1 MiB |
| Response frame including newline | 1 MiB | 16 MiB |
| Queued frames, plus one active | 16 | 64 |
| Retained prepared plans | 64 | 64 |
| Discovered tools | 64 | 128 |
| Tool catalog / individual schema | 1 MiB / 64 KiB | fixed |
| Plan token / serialized preview | 4 KiB / 32 KiB | fixed |
| Stderr scan per connection | 16 MiB | 1 GiB |
| Initialize / request / shutdown | 5 s / 30 s / 1 s | one hour each |

Metadata limits include JSON escaping; native registration imposes its own aggregate
catalog admission as well. The worker counts/discards stderr without archiving raw bytes.
Byte counters describe observed reads before closure, not a complete raw-output archive.
Only fixed failure classes, remote numeric error codes, exit/signal and cleanup/completeness
metadata reach diagnostics; arbitrary peer error messages/data are not copied into errors.
Complete RPC delivery does not certify that a server's output is semantically complete:
Completed.truncated remains a peer report, and core independently bounds/archives its result.

```sh
cargo build --locked -p aicoder-runtime --features process-extension-fixtures --bin aicoder-process-fixture
cargo run --locked -p aicoder-runtime --features process-extensions --example offline_process_extensions
```

The fixture is a separate fault-injecting Rust test executable, off by default, not a production server SDK. The host example registers it
through ExtensionSet, runs a scripted Agent with explicit approval, verifies the committed
file and acknowledged disposal of an unused plan, then confirms leader reaping. The fixture
prepares/commits the same runtime write backend, preserving conflict checks; no network,
model credentials, Node or Python are required. This is an SDK/embedded-host path; stock
CLI/TUI/Web launch/discovery awaits the package/profile/UI stages. Servers can share a
connection across tools, but active cancellation closes that connection for queued callers
as well. Hosts choose separate connections for independent lifecycles.

[Acceptance and footprint](s6-process-acceptance.md) records the verified scope and remaining S6 work.
