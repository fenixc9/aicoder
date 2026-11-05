# Provider-neutral model API (S1)

[Reasoning controls](reasoning-controls.md) add a typed requested effort and explicit model
declarations. Service default, disabled reasoning, visible reasoning history and billed work
are separate concepts; branch choices use validated metadata rather than prompt text.

`aicoder_core::model` defines model-facing data independently of HTTP, SSE,
terminal UI, and any provider's request format. `ChatClient` implements its
`ModelProvider` trait in `aicoder-providers`.

`TurnExecutor`, tools, sessions, CLI, TUI and evaluation use this contract.
OpenAI protocol types live only in `aicoder-providers::wire`; core retains a
private v1 session storage codec for compatibility. [Provider configuration](provider-configuration.md)
and [run outcomes and budgets](run-outcomes.md) are shared across hosts.

Optional [auxiliary classifiers](auxiliary-models.md) use the separate core
ClassifierProvider/ClassifierContext contract for native typed questions. Prepared tasks
share parent admission, cancellation and usage accounting without a nested chat loop.
[ImageGenerationProvider](image-generation.md) adds native image tasks and typed raw output
under the same auxiliary meter. Provider routes remain explicit host configuration.

## Types and ownership

- `Message` contains ordered `ContentBlock` values: text, reasoning, images,
  tool calls/results, or explicitly scoped opaque provider data.
- `Message::text_content()` joins visible text; it never exposes reasoning or
  opaque state as an ordinary answer.
- `ModelToolCall` has a JSON object for arguments. Providers assemble stream
  fragments and validate JSON before producing a completed call.
- `ModelRequest` names a model and contains messages, tools, choice policy and
  typed generation options. `ModelResponse` contains one assistant message,
  its stop reason and usage. There are no OpenAI `choices` or SSE types here.
- `ModelUsage` uses optional counts: missing means unknown. Start aggregation
  with `zero()`; adding an unknown count makes that aggregate unknown. Checked
  addition prevents overflow from masquerading as a valid measurement.
- `ModelSpec` describes capabilities and optional limits, prices and tokenizer
  identity. No tokenizer or pricing service is required by core.

`ModelRegistry` keys metadata by `(provider, model)`. `ModelProvider::model_spec`
selects an adapter's metadata, and `generate` accepts a request, event sink and
cancellation context. A custom provider only needs core, not a providers-crate
dependency. See the executable [in-memory example](../crates/aicoder-core/examples/model_provider.rs).

## OpenAI-compatible adapter

The generic profile declares tools, common sampling options and structured
output. Image input and reasoning history are opt-in there; DeepSeek profiles
declare the mode-specific history/sampling behavior described in
[provider configuration](provider-configuration.md). Metadata declares compatibility; it does not probe a remote
model or guarantee that every server implements the declared protocol.

Use `register_model` to override that fallback or add another model. The spec
must use the client's configured provider ID (`openai-compatible` by SDK default); duplicate registrations are rejected.
Capabilities should describe the actual endpoint and model. Unknown model IDs
fail before HTTP. Other adapters can use different provider IDs and profiles.

The adapter converts named tool selection, JSON Schema response formats,
mixed text/images and structured tool results into their wire representation.
Images in this adapter require user messages and HTTP(S) URLs or nonempty
base64 data with an image media type. A capability flag enables images;
validation of SDK-supplied image bytes remains the remote server's responsibility. High-level
Agent methods accept [AgentInput](image-input.md) for user text/images, and runtime offers
optional bounded local capture/normalization; neither introduces HTTP/UI concerns into core.

Reasoning returned by the server is a separate content block. Replaying it
requires an explicitly enabled `reasoning_history` capability and an endpoint
that accepts `reasoning_content`. Foreign opaque state is rejected by the
core validator; this adapter also rejects its own opaque state because the
chat-completions codec cannot preserve it. No state is silently converted to
visible text.

## Events, cancellation and errors

`ModelEventSink` receives transport-independent events directly; it should
return promptly. A host can forward events to its own bounded queue. The
normalized adapter does not start another dispatcher thread.

Each invocation emits one terminal `Finished` or `Aborted`. A request rejected
before HTTP can emit `Aborted` without `Started`. Streaming calls deliver
partial text, reasoning and tool arguments; non-streaming calls emit their
assembled content before `Finished`. Tool indices are local to a response.

Usage events are cumulative snapshots for one invocation, published as the
server reports them; replace the previous snapshot instead of adding snapshots
together. A final unknown snapshot is emitted if no usage was reported. The
executor aggregates one final response usage, or the latest failure snapshot,
per request. It never counts both events and the returned response as separate
usage. Publishing early also preserves reported counts if a host drops a
cancelled provider future.

Cancellation covers HTTP requests, retry waits and response streams. Errors
retain a structured kind, HTTP status when available, retryability and a
redacted message. A retryable error can be returned after internal retries are
exhausted; the flag describes the condition, not another automatic retry.

Empty successful answers, missing finish reasons, malformed/duplicate tool
calls and contradictory finish reasons fail explicitly. Token-limited and
filtered answers retain distinct stop reasons. Unrecognized wire finish
reasons are rejected by the current codec. The neutral `Other` variant is
available to adapters that can preserve additional stop reasons.

## Execution and next steps

`TurnExecutor::builder(provider)` injects any `ModelProvider` into the same
loop. The executor injects registered tools when a request has no tool list;
explicit tool definitions must match the registry. Tool-choice policy is
preserved and enforced before dispatch. Provider capability and response
validation protect custom providers as well as the built-in HTTP adapter.

Full transcripts preserve reasoning. For assistant history that the selected
model cannot replay, the executor projects display-only reasoning out of the
model-facing context. It retains opaque state and rejects incompatible state.
See [API migrations](migrations.md) for session v1/v2-to-v3 migration and result
changes, and [the embedded example](../crates/aicoder-core/examples/embedded_agent.rs)
for a full turn with a custom in-memory model, tool and store.

Structured [run outcomes and budgets](run-outcomes.md) are shared across hosts.
[Tree sessions and durable recovery](sessions.md) and [RunHandle controls](run-control.md)
and [versioned event subscriptions](events.md) complete S2; shared runtime and project
context are next in S3.

Tool messages can carry `ImageSource::Artifact` beside exactly one anchoring result. The
core host resolves these with an explicit `ToolImageResolver`; runtime offers an optional
scoped decoder. These sources are not valid user input. The compatible adapter projects
resolved previews after the complete tool-result batch, without adding user facts to the
journal. See [tool images](tool-images.md) and [migration](migrations.md).

Host root tool catalogs can be selected with TurnExecutorBuilder::model_tools. They govern
request declarations, preflight/context accounting and fresh direct model calls; the installed
registry remains available to explicitly scoped compositions. Narrower per-request subsets
also reject unadvertised registered calls before execution. See [root catalogs](code-mode.md#only-mode-and-root-declarations).

[Custom messages](custom-messages.md) add a canonical-only Custom role. Core counts and sends
a labelled compatible projection; direct adapter requests reject the unprojected role. Keep
typed custom entries as history and treat model_message() as a request-local view.
