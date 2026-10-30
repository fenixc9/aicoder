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
