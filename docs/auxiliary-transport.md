# Shared transport binding for native models

TransportBinding is an immutable native callback snapshot with a bounded shared monitor.
ChatClient, SystemOneClient and OpenRouterImageClient accept with_transport_binding; direct
with_transport_hooks/with_shared_transport_hooks remain convenient constructors. All three
use the same provider-owned driver for safe headers, allowed service controls, credential
isolation, callback cancellation and response observations. Core remains HTTP independent.

## Runtime assembly

```rust,ignore
let mut hooks = TransportHooks::new();
hooks.allow_payload_field("metadata")?;
hooks.register("host.trace", HookSpec::new("trace"), Arc::new(MyHook))?;
let binding = TransportBinding::new(hooks);
let runtime = runtime.with_model_transport_binding(binding.clone());
let provider = Arc::new(runtime.session_client()?);
let builder = runtime.executor_builder(provider)?;
```

with_model_transport_hooks constructs a binding directly. This explicit Rust operation applies
to clients subsequently created by Runtime::client/client_with_credentials/session_client and
to its native auxiliary registry. An already-created arbitrary ModelProvider passed to a builder
is not secretly replaced; bind that provider explicitly or construct it from the selected Runtime.
No setting, profile, project instruction, package state or history entry installs callbacks.

The binding is omitted from RuntimeConfig serialization and Runtime Debug. Runtime reload,
resource reload and validated package-state adoption retain the current host binding. Existing
clients/agents retain their own snapshot. A new Runtime loaded from the same files/history has
no callbacks until explicitly bound. An empty binding clears selection for subsequently created
clients. Replacing a binding selects a new monitor; clones of the old binding/clients keep their
previous monitor. Private backend handles and grants are never reconstructed from diagnostics.

model_transport_catalog/report expose selected metadata and bounded failures without constructing
an HTTP client. Binding/client catalog, allowed-field and report accessors are likewise read-only.
The monitor is shared across explicit client/model routes and reports request ID, provider/model,
owner/hook, phase and fixed failure class. Older serialized diagnostics default newly absent
provider/model fields to empty. No raw request, response, credential or native error text is
stored in this monitor.

## Native plan and usage boundaries

System One native bool/choice/score requests and image generation use the same prepared transport
stages after ordinary tool approval/admission. The accepted plan's state, questions, images,
assets and modalities remain immutable; related alternative root fields cannot be introduced
as uncounted input. TypeSafe's experimental image header is an owned protocol opt-in, not a
tracing header that callbacks may delete/change. Semantic native input changes belong before
owned preparation/approval, not in HTTP service controls.

Both native clients expose with_transport_hooks, with_shared_transport_hooks and
with_transport_binding. Known invalid preparation, expired admission or unavailable credentials
record zero native model usage before returning; cancellation with an uncertain request keeps
existing estimated/unknown accounting. They still make one request without implicit retries,
including HTTP errors, interrupted bodies and completed generation whose later delivery fails.
Reported usage is parsed before answer/image validation, and observer errors do not erase it.

Response observers receive the same safe, bounded, credential-redacted headers as chat. Native
auxiliary endpoints are non-streaming, so parsed-stream callbacks are not synthesized for them.
Shared callback children expire on dropped futures; the native caller's cancellation select owns
its request. Parent reservations, duration/tool/output limits, journal intent and actual usage
settlement stay in core. This binding neither grants tool permission nor changes unknown prices
into zero. Main and auxiliary subtotals remain separate while terminal usage includes both.

The common driver preserves the original clients' proxy/redirect/HTTP timeout and response-byte
policies. It captures native route proxy secrets for redacted observations. Native response
bounds remain two MiB for System One and the configured capped image response envelope. A raw
capture remains distinct from a decoded model preview. A callback cannot add IO/storage authority
by returning something shaped like an artifact.

## Runnable mixed-client proof

The loopback fixture uses separate main/classifier/image credential references, one shared
binding and actual compatible/native HTTP endpoints. It reloads settings/resources, runs the
agent through direct native tools or Code Mode, captures image artifacts and verifies source
history/usage. Allowing both native tasks produces four requests and 37 reported fixture tokens:
14 primary, 11 classifier and 12 image. Denial sends only two primary requests and admits no
native task. Observer failures populate the shared monitor while preserving completed results.
No real model, actual credential or image-quality experiment is used.

```sh
cargo run --locked -p aicoder-runtime --features auxiliary-models --example offline_auxiliary_transport
cargo run --locked -p aicoder-runtime --features auxiliary-models --example offline_auxiliary_transport -- --observer-error
cargo run --locked -p aicoder-runtime --features auxiliary-models --example offline_auxiliary_transport -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_auxiliary_transport -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_auxiliary_transport -- --code-mode --deny
```

Standalone auxiliary-models uses no JS/codecs. The JS variant uses the same registry, driver,
approvals and parent meter. See [transport contracts](transport-hooks.md), [native classifiers](auxiliary-models.md),
[image generation](image-generation.md), [migration](migrations.md) and [acceptance](s7-aux-transport-acceptance.md).
Actionable lifecycle hooks and final real-model/release acceptance remain in the [Pi audit](pi-parity-audit.md).
