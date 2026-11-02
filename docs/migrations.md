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
- `cargo run --locked -p aicoder-runtime --example offline_smoke`: real coding
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
Versioned bounded observation is now implemented; see [events](events.md).

## S2: canonical events and bounded observation

`AgentRawEventEnvelope` gains schema/version, producer time, optional session/turn and
terminal summary metadata. It serializes as protocol v1; `event` is a tagged object.
Use `AgentRawEventEnvelope::new` for external fixtures and fill required lifecycle
metadata when producing a complete trace. EventMeta and typed metadata events are now
Clone rather than Copy. Raw exhaustive matches gain SessionBound and
RoundOutcome::Stopped. `TurnExecutionConfig` gains `events: EventDeliveryConfig`;
use defaults in struct literals or configure callback count/bytes and drain timeout.

`RunProgress` adds `event_delivery` with a serde default; external literals must
supply it. Callback queues are now bounded. Slow/panicking/timed-out callbacks are
detached and reported there, preserving primary execution results.
`RunHandle`/`RunControl` expose bounded EventSubscriptions; overflow produces an explicit
error and a contiguous prefix. Dropping observation does not cancel execution. Callback
shutdown has a real-time bound and cannot forcibly preempt an in-flight synchronous
function. See [the complete lifecycle and limits](events.md).

`serde` gains its Arc/Rc feature and UUID gains serde support; no new Rust packages or
runtime services are introduced. The JSON Schema checker uses a separate development-only
Python requirement in CI and is not an agent/runtime dependency.

Evaluation trace JSON now contains canonical structured envelopes, replacing Debug
strings. Historical traces without schema version are not protocol v1. SWE-bench batch
reports move to schema 3 and include event_schema_version; checkpoints missing or
mismatching that value regenerate cases. v3 session file syntax stays unchanged.
Session terminal records retain their existing stats rather than claiming final
observer acknowledgements that can occur only after terminal publication.

## S3: shared runtime configuration

Applications now depend on aicoder-runtime for common settings/factories instead of
assembling provider/compaction/session defaults individually. Add the workspace crate
and use Runtime::load/load_with_environment, executor_builder, agent_config and sessions.
Core's model/execution interfaces are unchanged. EventDeliveryConfig::validate is now
public so hosts can validate settings before assembly. Builtin implementations/JSONL
remain in core for this increment; their S3 extraction has not been marked complete.

CLI max-rounds defaults are resolved by runtime instead of an always-present Clap
value; absence now allows environment/project/user configuration to apply. The shared
output default is 4096 (previous CLI default: 2048), and the base prompt replies in the
user's language. TUI defaults align with the same snapshot. ModelOptions now flow in
full through SweBenchAdapter::model_options; its existing temperature/max_tokens builder
methods still work. Bench parameters contain the frozen runtime snapshot, so incompatible
older checkpoints regenerate rather than mixing settings.

Automatic dotenv searches, compile-time crate paths and process mutation have been
removed. Migrate to --env-file /path/to/trusted.env or export variables explicitly.
Process values precede the private dotenv fallback. JSON settings require version 1;
project files cannot define profiles or session paths. Null differs from an omitted
field. Config/show-config commands work without bearer resolution and redact credentials.
See [paths, precedence, sources and reload boundaries](runtime-configuration.md).

Core BashTool::excluding_environment lets host assembly add managed credential reference
names. Runtime's registry uses it for selected/configured profiles; core's default unit
BashTool behavior remains compatible. Custom registries/credential providers retain their
own explicit environment policy.

## S3: project resource snapshots

Runtime resources are enabled by default on first resource/agent assembly; plain config
inspection remains lazy. AGENTS/SYSTEM/skill/template discovery is bounded and explicit
about roots, links, exclusions and source metadata. Use --no-resources or category flags
when automatic project context is unwanted. --no-config alone disables JSON settings,
not resources. See [resource rules and limits](project-resources.md).

Runtime prepares new user template/skill invocations before Agent/session submission.
Use prepare_prompt in SDK hosts; Core does not reinterpret historical user content.
TUI adds /resources and idle-only /reload. Existing agents retain their old generation;
changed SKILL.md bodies require a new snapshot. Directory handles/relative asset reads
replace arbitrary global-file access and do not execute scripts or grant permissions.

Core gains InputContextProvider/PreparedInput.system_prefix and optional input_context
metadata on RunProgress/RunTermination (serde default/omitted when absent). Literal users
must add the field. Only system prefixes may be supplied, preserving recovery/history
messages and avoiding history payload clones. RunJournal::record_input_context defaults
to no additional storage; custom journals can persist provenance. The bundled journal
adds aicoder.input_context extensions before model work. ToolContext::max_output_bytes
is public so injected bounded tools can honor the existing dispatcher capacity.

Runtime executor_builder binds its resource workspace. Bench/SDK callers previously
rebinding .workspace afterward should use executor_builder_for instead. SWE-bench uses
isolated base-checkout context, frozen globals and full source metadata; checkpoints now
also match base_commit and input_context_version. Incompatible historical checkpoints
regenerate rather than silently mixing contexts. Four Rust packages are added for real
bounded YAML parsing; shlex is reused and no script runtime is required.


## S3: concrete backends move to runtime

Default coding tools and `default_registry` move from `aicoder_core::tools` to
`aicoder_runtime::tools`. `JsonlSessionRepository` moves from core session to
`aicoder_runtime::session`. `WorkspaceChangeVerifier` moves from core/root completion
exports to `aicoder_runtime::verification`. Core retains all implementation contracts
and the memory repository. `ToolContext` path helpers move to runtime `WorkspacePaths`.

A bare `TurnExecutor` builder now installs no tools; explicitly pass
`.registry(aicoder_runtime::tools::default_registry()?)` to retain the coding registry,
or use `Runtime` assembly for settings/resources and credential-aware shell setup.
CLI, TUI and SWE-bench already assemble it explicitly. Approval still defaults to deny.
No persisted format change is needed: existing JSONL v3 files and legacy backups
retain their contract. Custom stores can attach opaque immutable view checkpoints
without adding backend-specific fields to core. See [the complete mapping and
examples](runtime-backends.md) and [acceptance](s3-backends-acceptance.md).


## S3: bounded discovery, reads and search

Runtime default registries append `list_directory` and `glob` after the original five
tools, including application settings/resource assembly. All four inspection tools are
read-only. `grep` now uses Rust regex and bounded asynchronous file reads instead of
collecting a ripgrep process's entire stdout/stderr. Existing `matches` remains display
text; paths are now workspace-relative and structured `records` carry line/byte positions,
excerpts and read cursors. Project-local `.gitignore` rules are honored; user-global
ignore rules and `.ignore` are no longer implicit process configuration. Set `hidden`
and `respect_gitignore` explicitly for other discovery policies.

Use `has_more`/`next_cursor` for all continuation. Read cursors can resume within long
UTF-8 lines and detect file changes; do not increment to the next line after a partial
fragment. Initial `offset_line` remains available with a bounded scan. Successful JSON
pages include their metadata/token in the output cap; insufficient budgets produce a
typed error instead of clipped JSON. Grep may return an empty but resumable scan-budget
page, and oversized lines include a recovery read cursor. See [the complete limits,
filters and compatibility rules](file-tools.md) and [acceptance](s3-file-tools-acceptance.md).
No core API, event schema or JSONL version changes are required.


## S3: prepare/review/commit for file mutations

`ExecutableTool::prepare` defaults to `Ok(None)`, preserving existing custom tool
implementations. Returning `Some(Box<dyn PreparedTool>)` opts into read-only preparation,
preview review and execute-once commit. The dispatcher invokes `execute` on that same
plan after approval; SDK users calling prepare directly supply their own approval policy.

`ToolInvocation` gains optional `preview`; add `preview: None` to explicit Rust literals.
`AgentRawEvent::ToolApprovalRequested` and its typed event gain the same optional preview;
add the field or use `..` in patterns. JSON event version remains 1, and older invocation/
event records without the field still deserialize. The schema and approval trace fixture
cover the additive fields. Existing custom approval callbacks need no new trait method.

Runtime write/edit now prepare exact bytes before approval and return `edit_conflict`
when originals change. Read pages and mutation receipts add `revision`; pass it as
`expected_revision` for an earlier-read guard, or `missing` for create-only writes.
Both mutation tools enforce a 4 MiB limit; identical replacement returns `no_change`.
Receipts must fit the output cap before writing. Diff previews can be explicitly truncated
or omitted. Normal creation uses 0600; replacement preserves lower permission bits and
creates a new inode. A post-publication directory-sync error is `durability_uncertain`
with `committed: true` and a receipt; inspect before retrying. See [the exact filesystem,
lock, cancellation and UI rules](file-mutations.md) and [verification/cost review](s3-edits-acceptance.md).


## S3: bounded shell lifecycle and output

`bash` retains `command`, optional `timeout_seconds`, legacy stdout/stderr/exit_code
and truncated fields. Optional timeouts now accept 1..86400 seconds and default to the
host context deadline. The shared `execution.tool_timeout_ms` / `AICODER_TOOL_TIMEOUT_MS`
setting (default 30000) is applied by CLI/TUI/bench factories and recorded in effective
configuration. Explicit settings take precedence; null/zero/over-one-day values fail.
Source literals of `ExecutionSettings`/`ResolvedExecution` need the new field or default
update syntax where supported. Host run budgets can impose an earlier stop.

Shell output is streamed into bounded prefix/tail storage; display truncation permits
normal completion, while a separate combined scan limit stops excessive producers.
The full output/details object fits the host cap (minimum 1024 bytes). New typed
`ShellOutput`/`ShellStopReason` expose counts, omitted bytes, capture completeness,
signal, stop reason and reaping/cleanup status. Nonzero exit, timeout and output-limit
failures retain captured details when Bash returns them; core interruption paths keep
their own existing outcome semantics. No JSONL or event-schema version changes occur.

All normal and abnormal completion paths request same-group cleanup. Background work
must be explicitly waited for; it no longer survives a successful shell leader by default.
New SDK `ShellLimits`, `ConfiguredBash` and public `registry_with_shell` support host policy
injection. Runtime credential-reference exclusions remain in place, with startup/function
imports suppressed. See [the complete lifecycle and environment contract](shell-tools.md)
and [acceptance evidence](s3-shell-acceptance.md).


## S3: typed decisions and transient session tool grants

`ApprovalHandler::decide(&ApprovalRequest)` supports `Deny`, `AllowOnce`, `AllowSession`.
The default wraps the legacy bool method; existing true values are one-shot and do not
create grants. New typed-only handlers can override decide; direct legacy approve then
defaults to deny. RunHandle adds `respond_approval_decision`; its bool method stays.
Unsupported session decisions are rejected without consuming the open question.

Hosts opt in with `TurnExecutionContext::with_approval_sessions`, using one bounded
cache per trusted principal/storage namespace. Agent binds the actual selected session;
custom transcript hosts can pass an explicit scoped handle instead. Grants apply to the
same workspace/session/tool/capability/registry and all arguments, stay in memory, and
are never restored from JSONL, model input or serialized snapshots. Scope epochs revoke
pending decisions; new registries do not inherit older tool permissions.

PendingRunApproval and ToolApprovalRequested gain optional session metadata;
ToolApprovalResolved gains optional resolution (decision/source/scope/grant ID), as do
typed event callbacks. Add fields or `..` to explicit Rust literals/patterns. JSON event
version remains 1 and older absent fields remain valid. No JSONL migration is needed.
CLI adds the explicit s choice for bound sessions; TUI adds s, /permissions and /revoke,
and clears scopes on successful reload/deletion. See [the full ownership, limits and
revocation rules](approvals.md) and [S3 acceptance](s3-approvals-acceptance.md).


## S4: canonical history and independent model context

`ExecutionInput` separates owned canonical history from the provider request. Existing
ModelRequest execution methods stay; explicit input/controlled/journal variants support
host projections. `Session::branch`/`selected_branch` provide borrowed views without
checkout, `model_context` adds origin/source metadata, and `context_for_active_turn`
requires complete live tool units. `ExecutionInput::from_branch` enables an alternate
branch run without persistence or selection changes.

Session-aware outcomes now start with recorded branch facts, rather than a repaired
provider view. Synthetic recovery results/system markers are model-only; raw arrival
order and real reasoning stay in history. Consumers forwarding result messages to a
provider should switch to safe projections. Canonical `input_messages`/journal positions
can differ from context lengths. Historical Compaction records keep their prior inert
semantics; automatic/manual semantic summaries remain future work.

Progress and terminal event-v1 summaries gain optional `model_context`. Add the optional
field to explicit Rust struct literals; old serialized data still reads. JSONL remains
v3. New reports expose actual adapted request counts and heuristic estimates, with no
extra message-body copy. See [the complete input, prefix and source rules](model-context.md)
and [verification](s4-projection-acceptance.md).

## S4 semantic summary checkpoints

JSONL remains v3 and event envelopes remain v1. New reserved extensions
`aicoder.context.summary.v1` and `aicoder.context.summary_usage.v1` hold validated
branch-local checkpoints and failed manual accounting. Existing legacy Compaction
records stay inert; no migration rewrites raw history. Older binaries ignore these
extensions and reconstruct full history, so use the updated binary to retain context
reduction and known manual-summary accounting. Do not downgrade expecting identical prompts.

`ContextCompactor` gains default `manages_budget`, `persistent_summaries` and
`compact_recorded` methods; existing implementations still compile. `RunJournal` gains
a default no-op `record_summary`; durable custom hosts should implement it and propagate
commit errors. New records use canonical retained-tail indices. Existing pruning and
plain SDK execution remain transient. SDK users constructing `ModelContextInfo` or
`SessionContextSource` literals must add the new summary fields (zero/None for old behavior).
Their optional serde fields read older JSON without changes; Rust source literals are
a documented source-level change. Runtime context settings gain optional strategy/output
fields; older JSON uses prune/1024 defaults. See [semantic-summary API and limits](semantic-summary.md).

## S4 output artifacts

`ToolSuccess` and `ToolFailure` source literals require `artifacts: Vec::new()`; constructors
keep empty defaults. `AgentRawEvent::ToolExecutionEnded` source literals also need the new
vector and exhaustive patterns should bind or ignore it. `ToolExecutionEndedEvent` exposes
the same references. Old event JSON omitting artifacts still deserializes, and empty vectors
are omitted; envelope version remains 1. ShellOutput gains optional reference/error fields.
JSONL stays v3 because tool results already hold arbitrary JSON; existing sessions remain
readable. References do not embed a private path or restore tool approval.

Core defaults retain no artifact store. Install one with `TurnExecutorBuilder::artifact_store`
and register `ReadArtifactTool` if the model should read pages. Runtime factories do this
when session storage is enabled; bare seven-tool registries stay unchanged. CLI `--no-session`
now explicitly clears the session directory and also disables persistent output artifacts.
Tool-end events include fixed bounded reference metadata even when a global output budget
cuts the preview. Generic oversized JSON previews are explicitly marked `json_prefix`, and
their serialized size (including escaping) fits the display budget. Failure details are now
bounded and may use that preview wrapper; consumers must not assume an oversized tool
result remains its original JSON type. Use the reference to retrieve exact returned JSON.
See [output/artifact limits and retention](output-artifacts.md).

## S4 session navigation and model selection

Navigation uses reserved `aicoder.session.navigation.v1` extensions; JSONL remains v3.
Rename/labels are global metadata over the complete log while model identity follows
ancestry. Older readers retain the records but ignore new names/labels; upgrading restores
their semantics without rewriting messages. New records are strictly validated; malformed
reserved records fail loading/commit. SessionNavigator operations require matching workspace
and an idle lease. Existing repository methods remain available to custom backends.

`ModelProvider` gains a default pure `validate_request` hook; existing implementations
compile unchanged. ChatClient adds explicit `for_model` without weakening its strict base
registry. Runtime's SessionModelClient enables fixed-service profile model selection without
routing credentials from history. Agent follows configured defaults unless the host opts
into `following_session_models`; CLI/TUI do so for continuation, with explicit CLI model/
profile overrides taking precedence. Input preparation/compatibility is now checked before
new session turn writes; unsupported history can fail without an extra user/turn record.
Preparation also counts toward duration budgets and is frozen once for execution.

ExecutionInput's new fields are private; constructors retain source compatibility. Existing
raw event shapes and source literals do not gain new fields. AgentStarted now reflects the
actual followed model. Model-selection reports are separate metadata-operation results,
not invented coding runs. Existing full `session-tree --json` output remains; bounded
inspection is `session-branches`. Scope checks now also cover old tree/migrate/delete CLI
commands. See [navigation/model constraints](session-navigation.md).

## S4 token counters and ContextPolicy

Core adds TokenCounter/TextTokenizer/TokenCounting and host ContextPolicy APIs; default
execution uses the same heuristic counts through a streaming writer. ContextCompactor
gains default supports_counting/compact_counted methods; old implementations still compile
and retain their trigger estimate. Their returned contexts are remeasured and must honor
counted targets/prefixes, or fail with preserved usage. Built-ins share the injected counter.

ModelContextInfo source literals need last_input_count/context_count/context_policy/
context_decision optional fields. Legacy JSON defaults them to None; actual SDK output
now reports provenance. ModelSelectionReport's existing optional token_count/decision fields
are populated. Event envelopes stay v1 and JSONL stays v3; the main event fixture now records
count/policy fields. Unknown-usage accounting remains estimated even with exact text
encoding; contradictory reported totals are floored by known reported component usage.

Runtime adds context.tokenizer/AICODER_TOKENIZER with heuristic/auto/cl100k_base/o200k_base.
Heuristic remains default. Explicit BPE requires tokenizer-tiktoken; auto exposes fallback
when unavailable or missing a model hint. Optional tiktoken-rs is lockfile-pinned and has no
runtime downloads. See [scope, accuracy and build costs](context-policy.md).

## S4 bounded archive windows

SessionRepository gains default-compatible open_run/archive_page/info/tree/search and
window commit/lease/navigation entry points. Existing custom stores retain Full defaults.
Runtime selects a window beyond its materialization threshold and bounds explicit open()
snapshots. Hosts needing the entire canonical archive must page it rather than assume
RunProgress.messages includes every old message. Source archived_messages and global
recorded_messages disclose the omitted raw prefix; generated/journal positions stay local
to the loaded canonical input, while summary anchors/counts remain global.

RunSession explicitly wraps Full or Window. Window backend checkpoints/ownership remain
opaque and nonserialized; committing invalidates its old projected context. Reload before
requesting a current model view. SessionNavigator.execute supports both forms; old Full
helpers remain subject to snapshot budgets. CLI adds session-archive; bounded tree/search,
continuation, model changes, summaries and TUI use the shared run/page paths.

SessionContextSource literals need archived_messages (zero for full history). SummaryDraft
literals need archived_raw_messages (zero), and CompactionRequest includes archived_messages.
Old JSON reads default zero and omits zeros, so event envelopes remain v1/JSONL v3.
Summary provenance distinguishes a materialized projection from all replaced raw records.
See [load limits, legacy behavior and IO tradeoffs](archive-windows.md).

## S5 CLI execution modes

Existing -p/--prompt remains the explicit prompt option; --print is final-text mode.
An optional positional prompt, bounded --stdin/automatic piped input, --json event-v1
output and --repl are now supported. Empty piped one-shot input errors instead of sending
the terminal-only default prompt. Metadata subcommands keep their own formats and reject
execution-mode flags. Tracing now writes to stderr. No event or JSONL version changes.

Piped prompt/REPL and machine/text-only modes do not consume payload lines as approvals;
noninteractive mutations require explicit --yes. Legacy explicit-prompt streaming stdin
approvals remain. Readers expire with their question receiver. Ctrl-C awaits cancellation/
persistence and exits 130 for one-shot runs. REPL uses core disk/memory sessions and resets
the context between turns. See [mode and compatibility details](cli-execution.md).

## S5 stdio RPC v1

`aicoder --rpc` is an alternative execution mode with versioned JSONL requests,
responses, core events and authoritative completion metadata. It conflicts with other
execution modes and metadata subcommands. Existing CLI and core event/session formats
are unchanged; RPC has its own version 1 envelope. Request IDs are strictly increasing
positive JavaScript-safe integers, including rejected commands, to prevent duplicate
execution without an unbounded ID cache. Clients must match run and approval identities.

EOF/shutdown/SIGINT cancel and await owned execution. A failed or stalled transport exits
nonzero after cleanup; clients must not assume a complete event stream after disconnect.
`--no-session` RPC uses an ephemeral core repository for conversation continuity.
See [RPC v1](rpc.md), [schema](protocol/rpc-v1.schema.json) and
[client example](../scripts/rpc_client.py). This protocol does not emulate Pi/JSON-RPC wire formats.

## S5 optional Web host

`aicoder-web` is a separate workspace package with an empty default feature set. Root
default members remain the six existing packages. Enable its `server` feature to build
the loopback HTTP/SSE/browser example; core/runtime/CLI/TUI dependencies and persisted
formats are unchanged. The lockfile records optional Axum dependencies for reproducibility.

The host chooses the workspace/provider and requires a private Web token before API body
extraction. Browser requests cannot select credential routing, private storage or tool
rules. Single-run admission, monotonic prompt IDs, exact approval identities and explicit
owner shutdown are part of [the Web contract](web.md). SSE disconnect does not cancel a
run or provide replay; inspect status or explicitly cancel. Embedded hosts must await
`WebHost::shutdown` before destroying their executor. Memory-session hosts must also clear
runtime session storage, preventing implicit artifact persistence.

Development source digests now include embedded HTML/JS/CSS alongside Rust/manifests,
so Web asset edits invalidate new baseline measurements. Historical reports retain their
original source identities. Browser verification dependencies remain development-only.

## S5 canonical JSON/HTML exports

`export-session ID --output PATH --format json|html` creates a new private, bounded
canonical snapshot containing all branches. `share-session ID --directory PATH` explicitly
invokes the directory publication adapter; an optional --base-url only constructs its URL
receipt. Existing files/symlinks are never replaced, storage is not migrated, and no model
credential or third-party upload is involved. Other destinations implement ExportPublisher
and receive the same immutable prepared bytes. No core API/event/session version changes.

Export schema v1 is distinct from session JSONL v3. It preserves domain entries rather
than copying raw transaction framing, private backend checkpoints, provider repairs or
ephemeral authority. Torn-tail completeness and unfinished turns remain explicit. Output
artifacts are referenced, not automatically bundled. Failures after file visibility expose
CommitFailure.visible; callers must inspect before retrying. See [the contract](session-exports.md).

## S5 declared reasoning controls

ModelOptions adds optional reasoning_effort, ModelCapabilities adds reasoning_efforts,
and ModelContextInfo/ModelSelectionReport add optional requested effort. Existing JSON
without these fields defaults correctly; absent options and empty declarations are omitted
on serialization. Rust struct literals must add the fields or ..Default::default().
ProviderProfile/RuntimeConfig also gain trusted models metadata; route identity must match,
and project files cannot define profiles. Defaults do not perform model discovery.

Explicit none is a declared capability; null/default omits the option. CLI/RPC/Web explicit
configuration overrides history; TUI treats configuration as initial fallback and validates
interactive branch changes. --reasoning-effort and set-session-reasoning expose host controls;
/thinking can stage a validated first-turn choice without creating a session or model call.
Generic models declaring controls use max_completion_tokens; classic empty declarations
retain max_tokens. DeepSeek compatibility profiles retain their fixed wire contracts.

Branch choices use aicoder.reasoning.v1 extensions inside existing JSONL v3. No new message
role/event type or authority is restored. Old readers can ignore the extension; updated
readers validate its model epoch. Event v1 gains optional requested-effort context metadata;
strict external validators must update the published schema. Requested effort does not
prove actual reasoning or billing. Capability/adapter checks precede user/turn writes.

Agent::with_reasoning_effort shares the live executor/registry while varying parameters;
rebuilding a registry for a preference change can invalidate ephemeral grants. TUI now
cancels and awaits owned operations after terminal EOF/errors as well as normal exit.
See [contract](reasoning-controls.md) and [acceptance](s5-reasoning-acceptance.md).

## S5 TUI interaction

The TUI now has a bounded grapheme/multiline composer, ephemeral accepted-input history,
explicit immutable /file pages, /detach, /usage and frontend palettes. Enter/Alt+Enter
retain steer/follow-up semantics; Shift+Enter or portable Ctrl+O inserts a newline.
Recognized bracketed paste no longer acts like typed Enter/approval keys. Oversized paste
leaves the draft unchanged. History recalls raw text rather than expanded templates or
restored attachments; actual canonical conversation storage remains core-owned.

RunControlSnapshot gains optional model_context metadata and RunHandle::model_context()
reads it without copying pending payloads. Explicit Rust snapshot literals add
model_context: None. No message/event/session format changes or authority restoration occurs.
Core updates these counts from the same projected request used for policy/budgets.
TUI base-rate estimates require known declared prices and complete usage; unknown/mixed
work remains unavailable. Palettes stay outside runtime/core settings.

/file uses the existing bounded read_file backend and preserves page revision/completeness/
cursor as user data. It does not fabricate a tool result, re-read captured pages on submit,
or bypass later model-tool approvals. Pending captures are cancelled on navigation/detach.
TUI preview clipping never guarantees recovery of failed archive writes. See
[interaction contract](tui-interaction.md) and [acceptance](s5-tui-acceptance.md).

## S5: unified text/image Agent input

Agent run methods now take impl Into<AgentInput> instead of impl Into<String>. Ordinary
String, &str and &String callers are unchanged. Wrappers generic over Into<String> should
pass a concrete String (`let text: String = prompt.into()`) or change their bound to
Into<AgentInput>. Explicit messages use AgentInput::try_from(Message); input roles and
blocks, inline base64 shape, image count and serialized image envelope are validated.
This does not introduce a session/event schema version: ImageSource was already part of v3.

CLI/TUI enable runtime's optional images feature. Bare core/runtime consumers keep their
prior dependency set unless they opt in. TUI controls explicitly allow 2 MiB input / 4 MiB
pending bytes; core default queue limits stay unchanged. Queued messages undergo model and
adapter validation before journal application; unsupported images remain pending on the
terminal result instead of appearing as applied user history. Image-aware heuristic counts
use explicit estimated allowances instead of counting base64 as text; visual/billed counts
remain provider-dependent. See [image input](image-input.md) for limits and migration examples.

## S6: explicit native extension snapshots

The additive `extensions` module registers native Rust tools, provider routes, context
policies, context/tool hooks, event observers and commands. Builders remain extension-free
by default. Install an Arc<ExtensionSet> with TurnExecutorBuilder::extensions, or use
Runtime::executor_builder_with_extensions. Provider/policy registration does not select
an implementation; the host explicitly looks it up. Contributed tool metadata is pinned,
and installing a snapshot forks the base registry with fresh grant authority.

InputContextProvider adds `prepare_with_prefix` with a default delegation to `prepare`.
Override it in prefix-composing providers to use the declared boundary. Core now calls it
for turns, model selection and manual compaction; runtime resources implement the boundary
explicitly. Native extension provenance wraps the prior provider metadata under `base`
and records extension manifests/context-hook IDs; this is opt-in and uses the existing
session input-context format. No session/event schema version changes or restored native
callbacks/grants are introduced. See [the contract](native-extensions.md) for execution,
callback delivery, timeout, error and cooperative native-code limits.

## S6: optional versioned process tools

Runtime's `process-extensions` feature adds ProcessConfig/ProcessExtension. The host supplies
an absolute program, fixed workspace, explicit environment and optional aliases/capability
overrides, then installs process.extension() into the existing native ExtensionRegistry.
The default capability is Mutating. Existing builders, settings and session/event formats
are unchanged; no project file or recorded model selection launches a program.

Core exposes the serializable process_protocol tool contracts without process/HTTP IO.
The JSON-RPC 2.0 stdio profile negotiates aicoder.tools.v1, with bounded prepare/call/commit/
dispose and no implicit retries. Plans/connection ownership remain private live state. New
snapshots rotate normal tool-registry authority; an old plan cannot migrate to a replacement
connection. Explicit shutdown reports reaping/incomplete cleanup; last-handle/future drop
signals owned groups and cleanup continues independently of Tokio. The shared owner also
backs Bash without changing its public API. See [the protocol/limits](process-extensions.md)
and [acceptance](s6-process-acceptance.md) before embedding a trusted external server.

## S6: optional MCP stdio profiles

Runtime's additive `mcp-stdio` feature reuses the bounded process transport and native
registration API. McpConfig owns MCP aliases/capability overrides/expected identity;
ProcessConfig supplies launch settings and its native-protocol override fields stay empty.
Auto probes modern server/discover before falling back to legacy initialize; an explicit
host protocol profile is also available. This does not make aicoder.tools.v1 an MCP dialect.

Catalog freshness and notifications trigger revalidation. Changed metadata invalidates an
old snapshot; install extension.refresh(...) through a new native registry/agent at the
host's chosen idle boundary. Media needs explicit scoped artifact storage; captured refs,
not base64 text or private result _meta, enter history. Missing capture is marked separately
from a completed operation, which is not retried. Legacy unsupported client requests are
bounded negative replies; no sampling/roots/elicitation/task capability is offered.

The shared transport now acknowledges notification writes separately from RPC responses,
and the MCP profile closes stdin before bounded termination/reaping. Native process tools
keep their strict profile and opaque-plan behavior. No new dependency/version or session/
event format is introduced. See [MCP contracts](mcp.md) for supported eras and host limits.


## S6: pinned local resource packages

Settings/RuntimeConfig gain `packages` (enabled + logical selections); project files cannot
set storage paths or executable profiles. Runtime resource manifests add optional nonempty
`packages` provenance and `ResourceScope::Package`; exhaustive downstream enum matches need
that variant. Existing manifests/generation calculation remain unchanged when no packages
are selected. Core adds a default `SessionRepository::extension_state` query; custom stores
need no change, while window stores may override to fetch real bounded branch records.
The JSONL implementation does so without materializing unrelated cold payloads.

No session/event/RPC schema version changes. `aicoder.packages.v1` is a bounded public
extension record whose content cannot install code or restore grants. CLI adds `packages`
subcommands, `--package-lock` and `--no-packages`. Rust hosts may opt into process/MCP package
activation with existing runtime features; stock frontends load text resources only. No
mandatory language runtime is introduced. Runtime activates the already locked ring 0.17.14
SHA-256 implementation for content integrity; core does not depend on it. See [contracts](packages.md).


## S6: scoped native UI and frontend command adapters

Core adds UI broker/data APIs without new dependencies or changes to persisted session/event
formats. TurnExecutionContext::with_ui is explicit; default contexts remain headless. Tools
access ToolContext::ui, and hooks/commands access ExtensionCancellation::ui. UI state is weak,
scoped, bounded and ephemeral; it cannot be restored through history or used as tool approval.
InputContextProvider::prepare_for_execution has a default legacy delegation, so existing
providers need no changes. UI-aware providers may override it. Model preflight, manual summary
and ordinary execution all use that path while preserving protected prefix boundaries.
Runtime::summary_agent_with_extensions also retains installed registrations for `/compact`.

The TUI adds a reusable library run_with_provider entry and `/ext [NAME JSON]`; templates
using `/ext` in that frontend now share a reserved built-in name and should be renamed.
WebOptions gains explicit native extension installation; authenticated command/UI routes
and additive request_id/ui/command snapshot fields use the existing Web protocol version.
Older clients may ignore those fields; the embedded assets adapt them. CLI/RPC do not implicitly
open stdin for UI; extensions use an explicit Unsupported fallback. See [UI contracts](extension-ui.md).

Interactive commands may declare up to 600 seconds; context/tool hooks retain the existing
30-second bound. No callback can bypass owner cancellation or UI admission limits.


## S6: prepared effects and coding extension examples

PreparedTool gains a default effects() -> PreparedToolEffects borrowed view. Existing plans
remain Unknown; no implementer change is required. Runtime write/edit replacements declare
their captured absolute targets. This is host metadata, not authorization, serialized domain
state or generic proof of side effects. Policy wrappers can fail closed for unknown operations
while retaining the same preview/owned plan and the backend's conflict checks. See the
[copyable planning, protection and worker examples](coding-extension-examples.md).

No default extension/tool, new dependency, session/event schema or mandatory runtime is added.
Read-only workers are explicit host command jobs with their own core budgets, canonical sessions,
trace and usage; they are not an unaccounted nested model tool or a restored authorization scope.


Host storage budgets are now configurable through ConfiguredBash::with_archive_timeout and
FileArtifactStore::with_lock_timeout, each with a 1 ms–5 s hard bound. Defaults remain 250 ms
and 100 ms. The additions preserve shell input/output/session formats and do not alter
process/pipe cleanup limits or retry completed operations. Explicit positive-test budgets
avoid mistaking slow disk finalization for a missing-archive implementation failure.


## S6: optional bounded tool composition

ExecutableTool has an additive default composition_factory method. Existing ordinary tools
need no change. Native wrappers forwarding a composed tool must forward this factory as well;
composition and a nonempty owned PreparedTool are mutually exclusive. Core's program protocol
owns no JS/HTTP/frontend dependency. Default builds and ordinary tool registration are unchanged.

Enable a host's code-mode feature explicitly and opt in through runtime settings or --code-mode.
Custom Settings/RuntimeConfig literals should use Default/update syntax or include code_mode.
The optional aicoder-codemode engine adds rquickjs/core/sys and relative-path; no loader or Node
runtime is enabled. Runtime and core do not require that dependency in default builds.

RunJournal adds record_composition, composition_state and commit_composition_state with default
implementations. Custom persistent journals must implement recording before allowing composition;
the default returns an unsupported error before any leaf operation. State commits also require
explicit support. SessionJournal and the explicitly transient () journal implement these methods.
Session-v3 storage needs no migration: real leaf facts and public branch state are Extension
records. Provider projection still contains the original outer assistant/tool pair only.
A composition interrupted after an external effect does not roll that effect back or retry it.
See [Code Mode](code-mode.md) for output, branch, cleanup and accounting semantics.


## S7: benchmark observations and grading semantics

TurnExecutionConfig and DispatcherConfig gain serde support. TurnExecutor exposes immutable
execution_config, dispatcher_config, extension_catalog and context_policy_name observations;
these methods grant no execution authority. Existing builders and application flows are unchanged.

EvalReport adds optional provenance, trace_capture and workspace_capture_complete (legacy JSON
without them loads as unavailable). A completed run without graders is now Inconclusive;
runtime failure or incomplete observation cannot produce Passed. Consumers must check
workspace_capture_complete before interpreting an empty diff as unchanged. Bounded post-run
snapshot failure retains core progress/usage/trace and yields an evaluator error.

AgentTraceRecorder defaults to a contiguous 100,000-event / 32 MiB prefix. Inspect capture_status
and never infer a complete run after overflow. AgentTrace::read_json validates bounded recorded
observations and performs no execution. WorkspaceSnapshot gains content/type/executable-bit
fingerprints, bounded capture and optional complete UTF-8 patches for non-Git fixtures.
Copy/capture exhaustion is an error rather than partial data. CommandEvaluator reuses runtime's
bounded Bash process owner; arguments must be UTF-8 and deadlines are at most 600 seconds.

Existing session/event/SWE-bench prediction formats do not change. Old SWE-bench generation
checkpoints retain their historical grading fields; this new benchmark archive format does
not import them or claim their old Passed flag is an official resolved test. Benchmark
manifest-v1 archives are exclusive, have no implicit resume, and verify their own input and
artifact identities. See [coding benchmarks](coding-benchmarks.md) for scope and Linux support.


## S7: owned host processes and SWE-bench schema 4

Runtime's process::capture exposes the existing bounded process owner to explicit Rust hosts,
using native OS arguments and an execution context. It installs no model tool. ShellStopReason
adds Cancelled (the enum remains non_exhaustive). Existing Bash tools keep their arguments and
approval path. Full raw archives can be requested by offering a scoped store, including for
small/binary output. CommandEvaluator now preserves native OS arguments instead of requiring
UTF-8 shell quoting; its old deadline bounds remain.

SWE-bench generation schema 4 records immutable plan/receipt/input/artifact identities and one
prediction row for every selected case, including failed placeholders. Supply an execution_pin
for resume. Legacy/incompatible/missing/corrupt receipts are refused without agent reexecution.
Use a fresh output directory for intentional regeneration; --no-resume never deletes an old
run. Unknown dataset IDs and path-like instance/job labels are rejected. Modern/future row
fields are preserved for hashes without becoming prompts. Async context-aware cache/batch APIs
support cancellation; synchronous entry points retain compatibility through an owned thread.

Official grading pins swebench==5.0.2 and its package contents. SweBenchHarnessConfig adds
optional generation_manifest, total_timeout_seconds and excluded_environment. The context-aware
async runner is preferred; the synchronous wrapper remains. Complete cache returns capture=None,
not fabricated process observations. Grading reports now use the full total/selected denominator;
submitted_resolved_rate preserves the separate legacy diagnostic. Linked grading requires the
same local dataset and a verified schema-4 generation file. Old generation/grade artifacts are
retained but never silently upgraded/reexecuted as current evidence. See [contracts](swebench-contract.md).
