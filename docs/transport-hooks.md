# Native HTTP transport hooks

ChatClient can bind an explicit immutable TransportHooks snapshot during Rust host assembly.
The snapshot adds before_headers, before_payload, after_response and parsed stream_event stages
to its existing compatible chat transport. SessionModelClient retains it through for_model;
Runtime can use that already-bound provider without a second execution loop. Core stays HTTP
independent. Settings, project data, resource records and restored sessions cannot load callbacks.

The driver also supports [native classifier/image bindings](auxiliary-transport.md) through
an explicit Runtime host snapshot. The public TransportSession interface remains available
for additional JSON-object drivers. Actionable lifecycle stages remain in the Pi audit.

## Assembly and authority

```rust,ignore
let mut hooks = TransportHooks::new();
hooks.allow_payload_field("metadata")?;
hooks.register("example.trace", HookSpec::new("trace"), Arc::new(MyHook))?;
let client = runtime.client()?.with_transport_hooks(hooks);
let provider = Arc::new(SessionModelClient::new(client));
let builder = runtime.executor_builder(provider)?;
```

with_shared_transport_hooks accepts an Arc snapshot for separate explicit clients. for_model
shares callbacks/report state and HTTP pools while retaining the configured service/credential
route. Replacing a snapshot creates a host-selected client; it does not mutate existing clients
or restore authority. transport_hook_catalog and transport_payload_fields expose metadata,
never callbacks or keys. TransportHooks is not deserializable. An extension can register a bound
client as an explicit provider contribution; installing a route does not silently select it.

TransportHook uses the existing HookSpec: 1–64 byte native IDs, ascending priority, stable
registration order for ties, and a 1 ms–30 s callback timeout (two seconds by default). At most
256 callbacks are admitted. Native code retains host permissions and must be cooperative,
read-only and Send + Sync; deadlines cannot preempt blocking Rust, process exit or arbitrary
allocation. Private cancellation tokens expire on callback return, panic, timeout or dropped
future without requiring async runtime cleanup. Callbacks receive observation-only cancellation,
not parent run/approval controls. Preparation also has a 30-second chain ceiling.

Each driver session has an admission/observation clock derived from the client's HTTP timeout.
Callback/attempt admission stops when it expires; completed buffered model data can still be
normalized. Existing per-HTTP request timeouts and configured retry waits remain distinct from
the owning core run's max_duration. The client yields/checks between synchronous preparation,
credential refresh and send, so parent cancellation/duration can be observed before new IO.
Use an explicit run duration when a total end-to-end bound is required.

## Ordered request stages

All before_headers callbacks run first, then all before_payload callbacks. They run once for
one logical request. The accepted safe header map and serialized payload are frozen and reused
by configured HTTP retries; callbacks cannot accidentally re-plan a paid inference per attempt.
A stable request ID correlates callbacks and diagnostics. Fresh bearer credentials are resolved
and injected privately for each attempt after header transformation.

The header view contains assembled application headers, not raw HTTP framing or credentials.
Return a map of name to Some(value) to set a header or None to remove it. Names normalize to
lowercase; duplicate normalized names, invalid names/values and CR/LF reject. At most 64 headers
and 16 KiB serialized header data are admitted. Authorization/proxy credentials, common API-key/
auth-token/cookie and organization/project routing headers, Host, framing, Content-Type and
Accept are protected. Use CredentialProvider/configuration for credential routing, not prompt
or header callback data. Backend transport/client defaults can add their own wire headers.

before_payload receives the complete encoded JSON object as immutable input and can return
None or an owned replacement. Every original adapter field must stay byte-value equivalent;
core-owned aliases for model, prompt/messages, tools, stream, sampling, output limits, reasoning
and formats cannot be added or changed. Only explicitly host-allowlisted extra fields can be
introduced, changed or removed by later callbacks. Additional data admits 64 KiB per replacement /
256 KiB across the chain; the full encoded payload admits 16 MiB. There are no default extra
fields. Allowed fields must be transport/service controls, not hidden extra model input or
unmetered generation. Core semantic context/options/tool edits belong in RequestHook before
validation/counting. This split keeps known window, root exposure and parent token admission
meaningful without pretending arbitrary service framing/billing is exact.

Preparation errors, invalid edits, panic or timeout fail before credential resolution/HTTP.
The normalized provider reports locally known zero model usage for a failed mutation preflight;
interrupted requests with uncertain send status retain normal conservative accounting. No raw
native error message, rejected value, auth header or payload is placed in diagnostics.

## Observation, usage and delivery

After response headers arrive, after_response runs for each actual HTTP attempt, including
unsuccessful statuses. It sees request identity, attempt number, status and a bounded safe
header view. Credential/cookie/challenge headers are omitted and known bearer/proxy values
are redacted even when echoed in other headers. headers_complete is false for omission,
non-text headers or exhausted view limits; the map is not an exact raw-header archive.

stream_event runs in order for parsed JSON SSE data, before adapter normalization. It retains
unknown provider fields that the typed codec may later ignore. It is an immutable credential-
redacted view, not HTTP bytes or the [DONE] framing marker. credentials_redacted identifies
changed content. Whole events admit 256 KiB; larger events or JSON values not representable by
the observation parser are omitted with explicit diagnostics, never delivered as truncated
complete JSON. The original typed codec still decides response validity and receives the
unmodified data. For example, an ignored extension containing an enormous JSON number can be
unrepresentable as Value without invalidating the model response.

Valid usage is published before awaiting raw stream observers, including on later cancellation
or malformed typed content when counts are available. Normalized cumulative snapshots deduplicate
that publication and final response usage. A callback cannot manufacture/drop usage or change
finish reasons, text, reasoning or tool arguments. Parsed-event omission does not truncate
model content or make missing usage free.

Observer errors, panics and timeouts are nonfatal diagnostics. They cannot alter normalization
or trigger an HTTP retry. With hooks selected, a successful HTTP status followed by an incomplete
body is an explicit delivery failure and is not repeated; an observer's delay must not repeat
completed/uncertain inference. Configured retryable statuses and pre-response transport policy
remain bounded and reuse the frozen plan; unsuccessful retry response bodies retain their
existing policy. Legacy clients without hooks keep their existing retry behavior. Service counts
and reports remain observations, not an invoice or a hard monetary cap.

## Diagnostics and custom drivers

transport_report returns at most 128 recent failure/omission records plus a dropped count.
Each contains request ID, owner/hook IDs, phase and fixed failure class, with no bodies or raw
errors. It is cumulative across for_model clones and explicitly shared bindings; use provider/model and request IDs to
distinguish concurrent calls. Reports are inert data and never reinstall hooks or credentials. Native state and private
payloads are not serialized or automatically recorded for replay. Hosts can explicitly bind a
separate bounded recorder when private IO evidence is required.

TransportSession::start/prepare/response/event/ensure_active/cancel/report provide reusable
owned stages for other JSON-object adapters. The driver owns actual IO, credential redaction,
usage publication before observation, and validation/attempt admission. Session Drop cancels
private children; cancellation input never gives a callback the session owner. Diagnostics and
whole-event bounds also apply to this path. Runtime now explicitly [binds the supported native auxiliary clients](auxiliary-transport.md);
custom driver use still does not turn observations into execution authority.

The runnable example composes core request views, the real compatible HTTP codec and a
SessionModelClient with tracing headers/allowlisted metadata. It verifies JSON/SSE, redacted
response headers, stable identity, original JSONL history and usage once. Only loopback and fake
credentials are used:

```sh
cargo run --locked -p aicoder-runtime --example offline_transport_hooks
cargo run --locked -p aicoder-runtime --example offline_transport_hooks -- --stream
```

See [request views](request-views.md), [migration](migrations.md),
[acceptance](s7-transport-hooks-acceptance.md) and the [remaining Pi audit](pi-parity-audit.md).
