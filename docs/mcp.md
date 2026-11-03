# MCP stdio (S6)

The optional runtime mcp-stdio adapter maps MCP tools into the same immutable native
registry, dispatcher, approvals, budgets, artifacts and canonical journal as built-in tools.
Launch authority remains explicit host ProcessConfig; model/project/session data cannot
select a program, environment or credential route. Remote transport and package/UI discovery
are separate work; this adapter launches no server in default builds.

Stock CLI/TUI/Web hosts can explicitly opt into [host launch files](host-extensions.md).

## Protocol profiles

The target is the official [2026-07-28 base protocol](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning),
with the documented [stdio compatibility probe](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio).
Modern requests use per-request version/client metadata and server/discover; the older
2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05 profiles use initialize followed by
notifications/initialized. An explicit host profile can select one era. Unsupported modern
versions/capabilities fail explicitly rather than being misclassified as a legacy peer.

The client offers no sampling, roots, elicitation, tasks or UI capability. Unsupported
server-initiated interactions cannot execute models, disclose roots, answer questions or
grant permission. Legacy ping requests receive bounded replies; modern peers do not issue
requests in that direction. Initialization instructions and peer metadata do not become
system prompts. Notification processing is bounded and cannot extend request deadlines.

Tools are discovered through bounded tools/list pagination. Input/output schemas and
annotations remain data; only explicit host overrides can classify a tool as ReadOnly.
Aliases retain the original remote name and avoid collisions/oversized model-facing names.
List-change signals and modern freshness hints trigger read-only revalidation. Changed
definitions invalidate an old snapshot; publishing new definitions requires an explicit
host refresh/new registry identity, preserving old run/approval authority boundaries.

## Calls, content and lifecycle

MCP tools have no native prepare/commit contract, so their actual tools/call request is sent
only after core approval. isError tool results remain tool failures; JSON-RPC failures,
unknown result types and unsupported interactions are diagnosed without implicit execution
retries. Pagination/discovery is read-only; uncertain completed/partial calls are never
replayed as if they had not happened.

Text and structured result data remain ordinary tool data. Resource links are not fetched.
Images, audio and embedded resource blobs use explicit scoped host artifact storage, keeping
binary base64 out of text-token estimates and metadata out of instructions. Capture failure
is distinct from operation failure: completed calls are not repeated to recover missing
media, and incomplete delivery is explicitly marked. Result-private _meta/continuation state
does not become canonical history or authorization. Raw media capture does not establish vision/audio billing precision or model visibility.
With optional runtime images and a declared vision model, [tool image projection](tool-images.md)
validates captured images and delivers bounded previews independently of raw capture status.

The adapter reuses the bounded process worker and leader/group ownership backend. Active
cancellation attempts the scoped MCP cancellation notification, then closes the connection
and terminates/reaps it so an ignored cancellation cannot leave a tool running indefinitely.
Initialization cancellation terminates the transport without cancelling an initialize RPC.
Explicit shutdown closes input before bounded escalation. Queue/frame/notification/catalog/
media limits are host admission bounds, not an OS sandbox. Servers are trusted programs
running with user permissions; detached processes and blocking kernel calls retain the
process backend's documented limits.

See the [MCP tool contract](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)
and [legacy lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)
for the two eras. The native [process protocol](process-extensions.md) remains separate and
keeps its existing strict framing/opaque-plan behavior.

## Rust host API and limits

Create McpConfig from an explicit ProcessConfig, connect it with an owner cancellation
context, then install extension.extension() into ExtensionRegistry/TurnExecutorBuilder.
ProcessConfig's native-protocol aliases/capabilities/expected_server fields stay empty;
McpConfig owns the MCP equivalents (including longer original names). Generated model
aliases are stable, bounded to 64 ASCII bytes and collision-checked. The original remote
name remains intact on the wire. Expected identity is an informational host check, not
cryptographic executable verification. Package pinning remains a separate responsibility.

| Resource | Default | Configurable hard maximum |
| --- | --- | --- |
| tools/list pages | 16 | 64 |
| Tools / catalog / individual definition | 64 / 1 MiB / 64 KiB | 128 / fixed / fixed |
| Content blocks | 64 | 256 |
| Aggregate decoded media per call | 512 KiB | 1 MiB |
| Auxiliary notifications/requests per second | 128 / 1 MiB | 1024 / 16 MiB |
| Queued legacy control replies | 8 | fixed |
| Auto-era probe timeout | 500 ms | 30 s |
| Host maximum catalog freshness | 300 s | one hour |

ProcessConfig retains the shared frame/queue/IO/cleanup limits. Modern TTL hints (including
zero) and legacy list-change signals cause bounded read-only revalidation before approval
and execution. Definitions stay immutable; an invalidated snapshot remains unusable even if
a server later restores earlier metadata. Queued tool calls also carry their validated epoch,
so a change observed before send rejects an unsent call. Refresh creates a new snapshot for
explicit host installation; it does not replace live agent definitions or restore grants.
Metadata caches are scoped to one fixed launch/auth/workspace context, never shared across
credentials. Schemas are retained and checked for bounded object shape; full JSON Schema
instance evaluation is left to host tool wrappers, and external schema refs are not fetched.

```sh
cargo build --locked -p aicoder-runtime --features mcp-fixtures --bin aicoder-mcp-fixture
cargo run --locked -p aicoder-runtime --features mcp-stdio --example offline_mcp
```

The optional Rust fixture negotiates modern or legacy profiles, then a scripted provider
uses MCP through a real core Agent. A trusted alternative server can be passed as argv to
this example, with `{workspace}` substituted into its host-owned arguments; it selects a
file-read tool over a temporary fixture and checks its returned text. This is a development
interop harness, not a model-controlled launcher. Rust tests and the default product need
neither Node nor Python. Stock frontend loading/discovery follows the package/UI stages.

Text, structured output, annotations and resource links remain tool data. Known binary
fields are decoded once into scoped artifacts and represented to the model by real host
references; audio/vision rendering and automatic image-to-model projection are not supplied
by this transport. Missing storage, capture limits or archive failures preserve completed
operation status with delivery_complete=false and no automatic tools/call retry. Private
result `_meta` and opaque MRTR requestState are omitted. Extra client interactions, MCP Apps,
tasks, prompts/resources RPCs and subscription streams are not advertised. Unsupported
interaction/result types carry explicit uncertainty, rather than pretending no work ran.

[Acceptance](s6-mcp-acceptance.md) records modern/legacy official-server interop, Rust fault
cases, approvals/budgets/trace and default footprint measurements.
