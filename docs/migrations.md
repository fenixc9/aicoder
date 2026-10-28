# API migrations

## S1: canonical model, execution and storage contracts

The HTTP client lives in `aicoder-providers`; core contains the provider-neutral
model contract and the single execution loop. CLI, TUI and evaluation now use
that same path. There is no legacy provider trait or second loop in core.

### Dependencies and imports

```toml
[dependencies]
aicoder-core = { path = "../aicoder/crates/aicoder-core" }
aicoder-providers = { path = "../aicoder/crates/aicoder-providers" }
```

```rust
use aicoder_core::{Agent, AgentConfig, TurnExecutor};
use aicoder_core::model::{ModelOptions, ModelRequest, Message, MessageRole};
use aicoder_providers::ChatClient;
```

Replace `ChatCompletionProvider` with `ModelProvider`: implement `model_spec`
and `generate`. Custom implementations only need core. Use
`model::emit_response` for a provider returning an assembled response; streaming
providers publish `ModelEvent` values through the supplied sink. The executor
also enforces cancellation around custom providers.

Core no longer exports `types`, `ChatMessage`, `ChatCompletionRequest`,
`ChatCompletionResponse`, or the old provider trait. Use `Message`, content
blocks, `ModelRequest` and `ModelResponse` from `model`. Direct HTTP callers
can explicitly import the protocol codec from `aicoder_providers::wire`.

### Requests, tools and results

- `ModelRequest::new(model, messages)` supplies defaults. Sampling parameters
  live in `request.options`; `stream` is a boolean. `AgentConfig` likewise has
  one `options: ModelOptions` field in place of the old flattened parameters.
- Tool definitions are plain `ToolDefinition` values, not nested function
  wrappers. Calls use `ModelToolCall { id, name, arguments }` with JSON values.
- Tool results are content blocks. `Message::tool_result` provides the first
  result's identity, JSON output and error flag. `text_content()` joins visible
  text without mixing in reasoning or tool output.
- `TurnExecutionResult.finish_reason` now contains a `StopReason`. Only a
  complete answer can pass completion verification; token-limited/filtered
  output is not reported as a successful turn.
- Run/event usage uses `ModelUsage` optional counts. Unknown counts remain
  unknown through aggregation. TUI displays `unknown`; evaluation report
  token fields are nullable instead of substituting zero.
- Model event usage snapshots replace the previous snapshot within one model
  request. The executor counts one authoritative usage per request. Failures
  retain the latest published snapshot; absent measurements remain unknown.
- Event `choice_index` fields are renamed `response_index`; actual OpenAI
  choices and SSE codec structs stay in the provider crate.

The executor preserves full reasoning in the transcript. When a model cannot
replay reasoning history, only the model-facing assistant context omits those
display blocks. Opaque provider state is retained and validated, never silently
converted to text. Direct model API calls still reject unsupported input.

### Sessions

S1 introduced version 2 canonical content blocks. S2 supersedes its storage behavior:
new sessions use v3, and the first mutation of v1/v2 creates an exact backup and
migrates automatically. See [tree sessions and legacy migration](sessions.md).

SWE-bench batch reports use schema version 2 for nullable token totals. Old
checkpoints without the canonical model contract version are not resumed into
new runs. Existing numeric token fields still deserialize as known counts;
new unknown values serialize as JSON null.

### Examples and verification

- `cargo run --locked -p aicoder-core --example model_provider`: direct in-memory model API.
- `cargo run --locked -p aicoder-core --example embedded_agent`: model/tool loop,
  in-memory custom tool and session storage, no model-service or file-edit I/O.
- `cargo run --locked -p aicoder-core --example offline_smoke`: real coding
  tools, cancellation and JSONL recovery with a scripted model.

Historical baseline measurements are retained in `docs/baselines/`; compare
like workloads and read their limitations before drawing performance conclusions.

The [canonical execution report](baselines/s1-execution-report.json) records a
44.85-second clean release build and 125 passing offline cases. Application
binaries are 10.96 / 11.34 / 11.23 MiB (CLI / TUI / SWE-bench), approximately
1.6–2.8% above S0. The offline runtime peak is 4.50 MiB RSS.

Its 470.6 ms runtime sample triggered timing review. All three
[repeat samples](baselines/s1-execution-repeat.json) are retained: 627.9,
292.9 and 300.9 ms, with a 300.9 ms median versus the historical 306.8 ms S0
sample. The first repeat included a release build immediately beforehand;
the later repeats reused it. These measurements do not establish the cause
of the outliers or a statistically significant speed improvement. They do not
show a sustained regression against the initial review budget.

## S1: retained progress and execution budgets

`TurnExecutionResult` now owns a `progress: RunProgress` field. Borrowing
`result.messages`, `result.usage` and `result.rounds` remains possible through
`Deref`; moves and struct literals use the `progress` field explicitly.
`TurnExecutionConfig` gains `budget: RunBudget`; use `..Default::default()`
when specifying only rounds/stream settings.

Prefer `TurnExecutor::run_outcome` to receive structured interrupted progress.
Existing `Result` methods attach `InterruptedTurn` to errors. Agent outcomes
add `Limited` and `Stopped` variants and expose `progress()`; exhaustive matches
must handle them. Event observers gain `AgentStopped`/`on_agent_stopped`, and
`AgentRunState::Stopped` is terminal.

Evaluation reports now contain progress and a typed stop, with limited/model
stops classified as incomplete. Token measurements are still nullable;
estimation lives separately in the budget accounting. See [the full contract](run-outcomes.md)
for scope, units, retry behavior and session persistence boundaries.

## S1: provider profiles and credentials

`ClientConfig` gains logical profile identity, credential references, connect
and response limits, proxy mode and compatibility controls. Existing struct
literals should use `..Default::default()`. Auto/system proxy is the new default;
set `ProxyMode::Disabled` or `AICODER_PROXY=disabled` for the prior direct mode.
The old cross-provider credential fallback is removed. See
[provider configuration](provider-configuration.md) for selection and defaults.

`CredentialProvider` supports request-time asynchronous token refresh. Model
metadata is scoped by `config.provider_id`, rather than always requiring the
literal `openai-compatible`. Normal model errors use the configured identity;
HTTP errors also expose optional vendor code/parameter fields.

CLI, TUI and bench use one provider resolver. Universal forced sampling
parameters have been removed; provider defaults now apply unless set explicitly.
DeepSeek thinking keeps required reasoning history and rejects known ignored
settings. Bench `--temperature` is now optional. Low-level wire response formats
serialize as protocol objects instead of null unit variants.

Shell commands now use non-login, noninteractive Bash and return captured
stdout/stderr. Commands inherit PATH but do not automatically source login or
BASH_ENV files. Provider secret variables are removed as described in the
configuration guide. Broader tool environment isolation remains S3 work.

## S2: session trees, ownership and durable execution

`SessionRepository` is object-safe. Implement `commit`, `acquire_run`, and `migrate`
in custom stores alongside create/open/list/delete. Default append methods commit
one atomic `SessionTransaction`; `append_all` batches messages rather than looping
through independent writes. Enforce ancestry validation, revision conflicts and
single-run ownership consistently. `Session::from_entries`, `validate_transaction`
and `apply_transaction` support external domain reconstruction and folding;
`run_token`/`attach_run_token` carry ephemeral ownership. See the bundled stores
and shared contract tests.

`Session::messages()` now returns borrowed `SessionMessage` views over the selected
branch; use `entries()` for the entire immutable tree. Each entry has a parent and
`SessionEntryData`; metadata adds selected leaf and revision. Struct-literal users
must update for these fields and the revised `SessionInfo`. Use `chat_messages()`
only for raw display/export, and `model_messages()` for continuation.

Agent session methods accept `&dyn SessionRepository`; ordinary calls remain the
same, but explicit generic/turbofish calls should remove their type parameter.
Agent runs acquire an owned lease, checkpoint tool calls/results, and save terminal
metadata before final events. `InterruptedTurn` and `InterruptedAgentTurn` add an
optional boxed `persistence_error`; custom literals must supply it (`None` normally).
Partial fragments are saved as turn metadata and displayed separately from messages.
The lower-level executor offers optional `RunJournal` injection via `run_with_journal`.

v3 JSONL uses checksummed transactions, not individual message records. Read-only
v1/v2 open stays unchanged; explicit `migrate` or first mutation preserves exact
`.v1.bak` / `.v2.bak` originals before atomic replacement. Stop older clients first.
The [session guide](sessions.md) documents rollback, corruption and uncertain tool
outcomes, along with new CLI branch/inspection/migration commands.

## S2: RunHandle controls

New controlled methods consume a unique `RunControl`; the host keeps a cloneable
`RunHandle`. Existing context-only methods remain available. TUI uses core-scoped
approval IDs and accepts steering/follow-up while running. See [run control](run-control.md)
for lifecycle, bounds, safe points and pending-input recovery.

`RunProgress` and `TurnRecord` add `pending_inputs` (serde default/omitted when empty).
External struct literals need the field; older serialized outcomes/v3 records remain
readable. `RunJournal::input` has a default checkpoint implementation; override it to
retain input provenance in a custom journal. Raw event matches must handle the new
`RunInputApplied` and `RunApprovalRequested` variants; typed callback defaults are no-ops.
The underlying event queue is not yet bounded/versioned; that remains the next S2 item.
