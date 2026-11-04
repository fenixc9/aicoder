# Native extension API v1 (S6)

An Extension is an explicit set of Rust implementations compiled into a host. The host
installs it in an ExtensionRegistry, checks collisions/budgets, then freezes an ExtensionSet.
TurnExecutorBuilder::extensions or Runtime::executor_builder_with_extensions installs that
snapshot in the existing loop. Default builders install none. No settings file, project
instruction, model message or restored session can load native code or select its credentials.

The manifest records a bounded ID, opaque version and api_version = 1. This is a logical
registration contract, not a Rust shared-library ABI. Source compatibility follows the crate
version. [Process tools](process-extensions.md) use an optional runtime adapter over this
registration API; [MCP stdio](mcp.md) adds modern/legacy tool profiles. Package
[Local resource packages](packages.md) provide pinned manifests/install/reload; executable
activation remains a separate explicit host operation.
Native code has the host process's permissions and can access std/IO directly. These contracts
bound cooperative callbacks and admitted data; they cannot sandbox arbitrary Rust code,
preempt a blocking callback or recover from process exit/abort/allocation failure.

## Registration and authority

Contributions include ExecutableTool, ModelProvider routes, named ContextPolicy objects,
ContextHook, RequestHook, ToolHook, ToolCallHook/ToolResultHook, AgentEventHandler observers and ExtensionCommand. Existing public
traits stay authoritative; no parallel execution loop or provider-specific core types appear.

- IDs/names are 1–64 ASCII letters/digits/dot/underscore/hyphen; versions are 1–128 graphic
  ASCII bytes. At most 32 extensions / 256 contributions / 1 MiB admitted metadata are kept.
  Individual tool/command definitions are capped at 64/32 KiB serialized JSON and require
  schema objects. Native objects' own memory is host-controlled and outside metadata limits.
- An install validates the whole extension before publishing anything. Tool/provider/policy/
  command names are unique across the set; hook/observer IDs are local to their extension.
  Hosts reserve built-in command names before installation. No silent replacement occurs.
- Contributed tool definitions/capabilities are captured at registration. Installing into a
  base ToolRegistry checks collisions atomically and creates fresh registry authority.
  Existing agents retain their snapshot. Native callback state may still be explicitly
  shared/mutable by the host; the catalog is not a serialization of native objects.
- The host explicitly looks up a provider route or context policy. Registering one does not
  activate it. Recorded model choices can select models only within the current host-selected
  provider route. Policy selection cannot bypass core's known windows or protected tool units.
- Tools still use dispatcher preparation, owned previews/plans, approvals, cancellation,
  output/artifact bounds, accounting and journal checkpoints. Extensions cannot turn a
  command reply or stored catalog into an approval or restored session grant.

## Interception

[RequestHook](request-views.md) additionally owns request-local conversation/system views,
generation options and narrower tool declarations before admission. Explicit full-prefix scope,
prepared executor-bound snapshots, raw-history compaction/rebuild and safe scalar receipts
keep these views separate from canonical facts. Explicit [provider transport hooks](transport-hooks.md)
are assembled on a compatible client before registering/selecting its provider route. They do
not introduce HTTP types into core or load from extension catalogs. [Shared Runtime binding](auxiliary-transport.md) covers its native auxiliary clients. [Boundary hooks](boundary-hooks.md) stage inert data or
explicit context at successful round/settlement boundaries and can continue the same metered run.
Input/start/message transforms and session navigation veto remain open.

HookSpec defines local ID, signed priority and a 1 ms–30 s timeout (default two seconds).
Lower priority runs first; ties preserve extension installation/contribution order. Calls
are sequential within one chain; separate runs/read-only tool dispatches can overlap, so
implementations must satisfy Send + Sync. The first denial/failure stops that chain.

ContextHook runs once per input-preparation pass and returns optional additional system
text. Model-selection and manual-compaction previews also prepare inputs, so callbacks
must be read-only/idempotent; a prepared turn freezes and reuses its result. Its immutable view contains the prepared base prefix, prior contributions and
canonical input history. It does not expose backend checkpoints or rewrite user/tool facts.
Each contribution is capped at 32 KiB JSON, with a 64 KiB aggregate; the context phase,
including base preparation, has a 30-second cooperative limit. Core's run budget/cancellation
can stop it earlier. Model options/tools, history, recovery markers, provider route and
authorization are not mutable fields. Prefix/provenance is frozen before admission, protected
by normal context policy, and recorded through the existing input-context journal mechanism.

InputContextProvider::prepare_with_prefix supplies core's declared runtime-prefix boundary.
Extension composition and runtime resources use it instead of reclassifying leading derived
system messages as host input. Its default delegates to prepare for source compatibility;
custom providers that compose prefixes should override it. Historical provider projections
and canonical journal indices remain separate.

[ToolCallHook/ToolResultHook](tool-interception.md) additionally provide bounded argument and
delivered-result transformations with inert proposal/execution receipts. Their actual tool
identity, operation completion, capture facts and artifact authority remain explicit.
ToolHook runs on every resolved tool call, after argument transformations and before read-only preparation or any approval/cache
lookup. It sees workspace, effective call/arguments and declared capability, and returns
Continue or Deny with at most 4 KiB JSON reason. It cannot rewrite IDs, names, arguments,
results, previews, plans or capabilities; later approved execution consumes the original
owned plan. A Deny becomes a paired extension_denied tool result. Callback errors/panics/
timeouts become paired extension_hook_failed results. The whole tool-hook phase is also
limited by the dispatcher's tool_timeout. Denials still apply to calls with session grants.
New snapshots receive new registry identity, so old grants do not silently carry across them.

ExtensionCancellation exposes cancellation observation and scoped [host UI data](extension-ui.md), without approval/run-control authority or
the ability to cancel the owner. Timeout, cancellation, steering or dropping a callback
cancels its private token and drops its future; late child work cannot publish a hook result.
Implementations that spawn tasks must honor that token. A callback can otherwise retain its
own host resources; it must keep mutation out of context/guard preparation. Errors returned
to the loop identify extension/callback and failure class without copying raw error strings.
Rust's process panic hook remains host-owned and may run before a panic is caught.

## Observation and commands

Observers return no decision or mutable message. The host event handler runs first, then
registered observers in priority order; raw and typed dispatch happen once for each.
They share the existing bounded callback queue and callback_drain_timeout. Overflow, panic
or drain timeout is explicit in RunProgress.event_delivery and does not authorize, abort or
rewrite execution. Failure stops that callback consumer's remaining prefix; independent
EventSource subscribers continue. A blocked native callback may outlive the bounded drain;
the report does not claim that its thread was killed or its external side effects undone.

ExtensionCommand receives explicit arguments, workspace and optional session metadata.
It returns Notice text (64 KiB JSON) or a validated user AgentInput (2 MiB minus 16 KiB),
with 64 KiB JSON input arguments. Commands declare a 1 ms–600 s deadline for human interaction;
context/tool hooks retain HookSpec's 30 s ceiling. Commands use the same bounded cooperative callback/error rules
and owner cancellation. The host displays the notice or explicitly sends the returned input
through its ordinary Agent/session/approval path. Execution never starts implicitly.
Arguments are untrusted; commands validate their own schema-specific semantics. The API
does not give commands an approval rule, hidden session mutation handle or automatic tools.

The SDK exposes command specifications and execution. Explicit native TUI/Web hosts now
adapt commands through `/ext NAME JSON` and authenticated Web command routes; see [UI contracts](extension-ui.md).
Headless CLI/RPC do not implicitly open stdin. `/plan` in the offline Rust example remains
an embedded-host command, not a new built-in slash command in stock binaries.

```sh
cargo run --locked -p aicoder-core --example native_extensions
```

The example registers provider/tool/policy/context/guard/observer/command contributions,
submits a command result through Agent and proves that planning blocks mutation and leaving
planning does not create approval authority. Runtime integration tests additionally compose
native context with project resources and JSONL sessions. No credentials/network are used.
