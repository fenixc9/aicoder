# Native request view hooks

RequestHook transforms an ephemeral provider-neutral ModelRequest before context admission,
compaction decisions and generation. Conversation filtering, explicit system-prefix replacement,
generation options and tool subsets use one core path for SDK/frontends/bench hosts. Canonical
user/tool history stays unchanged. This covers the pinned Pi context/context_with_system intent;
[Compatible-client transport hooks](transport-hooks.md) add safe headers/service controls and
response/parsed-stream observations. [Shared auxiliary transport binding](auxiliary-transport.md) now covers Runtime-created native
models. Actionable lifecycle hooks remain separate open audit items.

## Register a bounded transformation

```rust,ignore
extension.request_hook(
    HookSpec::new("request-view"),
    RequestHookScope::Conversation,
    Arc::new(MyRequestHook),
)?;
```

Callbacks run in ascending priority, with stable installation/registration order for ties.
RequestHookInput contains workspace, model spec, canonical history, current request, declared
runtime-prefix length, round and pass. Return None or a RequestEdit; later callbacks see earlier
edits. These callbacks are read-only/idempotent and can run during model-selection previews,
turn preparation, admission or post-compaction rebuilds. They cannot execute an agent loop or
acquire authority from their input. Trusted native Rust retains host permissions; cooperative
limits cannot sandbox blocking code or memory allocated inside a callback.

RequestEdit has optional messages, options, tools and tool_choice. Model identity, provider
route, credentials and stream mode are not writable. Conversation scope preserves the exact
current runtime prefix and its declared length. SystemAndConversation explicitly owns prefix
replacement, requires system-role prefix messages and preserves a system prefix when one was
present. Historical/derived system markers are not reclassified as runtime instructions.
The host's newly selected prefix remains protected in the request view; raw summary history
keeps the original prefix and canonical facts.

Tools are unique names selecting an unchanged subset of the preceding view's host definitions;
removed tools cannot be reintroduced by a later hook. An explicit empty list remains empty
through provider encoding and root dispatch. Hidden direct model calls are refused normally;
composition keeps its separately allowlisted leaves. Generation options and tool choice must
match declared model capabilities and adapter rules. Each edit must leave a valid complete
request, including paired tool units and provider-specific history. A conversation transformation
is not a new canonical user action, selected model or restored authorization rule.

A view admits at most 16 MiB serialized JSON, with a 32 MiB aggregate budget across replacements.
An options replacement admits 64 KiB; tool-name lists admit 1024 entries. Callback deadlines use
HookSpec's 1 ms–30 s bounds (two seconds by default), with a 30-second chain ceiling. Parent
cancellation/duration can end work earlier and private callback children expire on drop/timeout.
Raw native error strings are not copied into run errors. The default path with no request hooks
retains borrowed context admission and installs no callback, transport or vocabulary dependency.

## Preparation, admission and compaction

Session turns and model switches call prepare_input before committing user/model metadata.
It prepares host resources, projects provider-compatible history and freezes the first hook
view. validate_model_selection uses that owned view. Callers using that synchronous check with
installed request hooks must first await prepare_input; it cannot secretly run async callbacks.
Prepared views are private, in-memory and bound to the executor instance. They are neither
serialized nor restored as authority. A different executor discards the snapshot.

The first active round reuses its frozen view when no queued input was applied. Applied controls
invalidate/drop it and trigger a fresh view. Later rounds start from the unmodified host options,
tool catalog and raw/reduced context, not a previous hook's output. Incoming controls follow the
ordinary run boundaries; request views never grant approval or bypass tool steering checks.
Request-local reasoning changes do not become branch reasoning selections. ModelContextInfo's
selected effort retains the host preference; receipts observe effective request scalars.

Core counts the transformed view before deciding whether it fits the window. Filtering can
therefore rescue a large context instead of being blocked by a pre-transform count. A custom
policy cannot keep a request beyond a known window. Adapter/capability checks and parent token
admission run again against the final materialized view, including scoped tool-image delivery.
The final generation allowance is still clamped by the owning run's remaining token budget.
Elapsed budgets are rechecked after hook/receipt work and before summary/main provider
invocation, including when native synchronous work prevented the async timer from polling.

When a transformed view needs automatic compaction, the compactor receives raw context and
canonical history. It never summarizes injected hook messages as historical facts. Summary
attempts retain ordinary usage accounting, raw branch anchors and durable checkpoints before
reduced context is used. Core then rebuilds the request from the base host configuration and
reapplies hooks with AfterCompaction. One bounded compaction pass occurs per round; a view still
requiring compaction or rejected after rebuilding/materialization fails before main generation.
Hooks that add substantial context must leave room for it in the configured window. There is
no unbounded reapply/summary loop or automatic retry of completed inference.

Read-only selection/preparation does not call a provider or write projection receipts. The
actual run records Admission before compaction/provider IO and Ready before main generation,
even when installed callbacks return None. Custom journals must support these records before
using the feature, so an initially unchanged view cannot defer a support failure until after
summary IO. The explicit unit journal supports ephemeral SDK runs. Journal waits are bounded,
cancellable and retain persistence failures without retrying provider operations.

## Observations and history

Session v3 stores inert aicoder.request-view.v1 Extension entries; eval journals retain the same
RequestViewRecord. They carry stage/pass/round, provider/model identity, source/view counts,
prefix boundary, effective tool names/choice, hook changes and shared TokenCount precision.
They record intent/ready views, not proof of an HTTP request, billed tokens or completion.
Canonical journal positions still follow actual user/assistant/tool records. Old readers keep
unknown entries inert; resume never loads callbacks, routes, grants or private caches from them.

RequestOptionSummary observes effective reasoning, temperature, top_p, max_output_tokens, seed,
stop count and response-format kind. Ready observes the allowance after parent clamping. It
never copies stop strings, JSON schema names/bodies or request messages into history. complete
is false when those omitted option values prevent complete reconstruction. Even a complete
options summary is not a full provider-payload fingerprint, service-default/billing estimate or
claim that the model honored its requested parameters. Native callback state and private view
text are outside this metadata-only observation; hosts needing exact private replay must own
and scope a separate recorder. These inert observations cannot select model routing.

## Executable HTTP example

The loopback example installs an explicit full-prefix hook, replaces only the outgoing prompt,
sets generation scalars and removes all root tools. It uses the actual compatible HTTP adapter,
checks request bytes for ordinary JSON and SSE, verifies reported usage is counted once and
retains original user text plus safe receipts in JSONL. It uses fake credentials and no real
model service:

```sh
cargo run --locked -p aicoder-runtime --example offline_request_views
cargo run --locked -p aicoder-runtime --example offline_request_views -- --stream
```

See [native extensions](native-extensions.md), [migration](migrations.md),
[acceptance](s7-request-views-acceptance.md) and the [remaining Pi audit](pi-parity-audit.md).
