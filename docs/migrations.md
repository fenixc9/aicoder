# API migrations

## S7: guarded session navigation and host lifecycle

Extension adds session_hook(HookSpec, Arc<dyn SessionHook>). before receives immutable resolved
operation/source/target positions and may Continue or Deny. after observes actual lifecycle events;
its failures are bounded delivery data and cannot veto/retry an adopted change. See [contract](session-lifecycle.md).

SessionNavigator adds prepare; PreparedSessionNavigation retains the lease and exact transaction.
Agent::navigate_session / apply_navigation validate branch compatibility, call native guards and
commit those entries. SessionRepository adds prepare_window_navigation and commit_window_navigation
with unsupported defaults. Custom window backends must implement these for guarded operations;
legacy navigate_window/SessionNavigator::execute remain available. JSONL and memory paths are covered.
No plan or preview is serializable, and backend/lease tokens are not exposed to hooks.

Agent::select_session explicitly adopts new/resumed sessions and reports Started/Shutdown.
notify_session_shutdown / notify_session_reload are host notifications after owner settlement or
runtime adoption. run_hosted_session_outcome / controlled wrap the same agent loop with explicit
host adoption and return HostedSessionOutcome {outcome, lifecycle}. A private preparation clock
charges adoption waits to the same run. Acknowledged creation metadata survives subsequent aborts
or budget expiry; SessionCreatedWithoutOwnership reports an already-created session if ownership
could not be acquired. No automatic creation retry occurs.

Ordinary Existing turn APIs continue recorded sessions without inferring a new host binding.
Low-level storage APIs are trusted host primitives, not a session-policy sandbox. CLI, RPC, TUI and
Web hosted runs use the explicit adoption path when callbacks are installed. Native RPC selection
is asynchronous and resolves New to its actual ID; cancel_session_selection {request_id} targets
only that owned request. IDs remain monotonic, bounded and non-replayable. Native run/selection
results may include session_lifecycle reports. No callback, model route, queue or tool grant is
restored from these observations. TUI navigation, new/resume and active-session deletion settle
their owners before adopting UI state; denied changes preserve drafts and existing authority.

Session versions, dependency manifests, lockfile and default features are unchanged. See
[acceptance](s7-session-lifecycle-acceptance.md). Fork/clone into a distinct session, tree-summary
customization and compaction interception/drafts remain separate audit items.

## S7: general fresh-message finalization

Extension adds message_hook(HookSpec, Arc<dyn MessageHook>). Its ordered immutable input identifies
origin/role/round, original/current content and optional custom metadata. It runs after user raw
hooks/expansion, specialized assistant finalizers or specialized tool-result edits, as applicable.
Fresh Agent inputs, custom idle/next-turn/queued delivery and native startup/boundary context are
covered. Existing ModelRequest/ExecutionInput histories remain histories. See [contract](message-finalization.md).

RunJournal adds record_message_finalization and finalize_message with unsupported defaults. A chain
probes support before provider IO; commit must atomically bind effective content, safe receipts,
the canonical index and any input/assistant companion metadata. SessionJournal, explicit transient
`()` and EvaluationJournal support it. Initial Agent session writes and context boundary writes
include their finalization receipts in the existing atomic transaction. History restores data only.

BoundaryCommit adds message_finalizations with serde default/empty omission; update Rust literals
with Vec::new(). Session versions stay unchanged. Receipts use inert Extension records under
aicoder.message-finalization.v1; original content is not stored in receipts. Captured originals of
completed operations survive failures in normal progress/history when storage remains available.

AgentRawEvent adds MessageFinalized {message, custom, origin, changed}; typed handlers gain a default
on_message_finalized and the v1 schema validates origin/role/custom metadata pairing. Assistant
finalization also emits the existing AssistantMessageFinalized event for frontend compatibility.
Use only one as your assistant display trigger. Other changed messages are labelled explicitly in
stock frontends; hidden custom metadata stays hidden. Streaming/earlier tool observations remain
previews, and event delivery cannot assert that every observer received them.

Tool status/artifact edits belong to ToolResultHook, before MessageHook. Generic tool edits preserve
call identity, delivered status, error class/details, capture fields and typed media; output/message
text may change within owned limits. Assistant actions/opaque state and custom/context source labels
remain protected. Runtime instruction prefixes use BeforeStartHook rather than message finalization.
See [acceptance](s7-message-finalization-acceptance.md).

## S7: assistant message finalization

Extension adds assistant_message_hook(HookSpec, Arc<dyn AssistantMessageHook>). The input includes
immutable original/current messages, response usage and stop reason. Only text and visible reasoning
may change; role, actions and opaque state remain exact. There are no default callbacks or features.
See [contract](assistant-finalization.md) and [acceptance](s7-assistant-finalization-acceptance.md).

RunJournal adds record_finalization and finalize_assistant with unsupported defaults. Custom journals
with installed finalizers must acknowledge support before provider IO and atomically persist a safe
receipt plus the effective message at its unsaved index. Later checkpoints/finish must not duplicate
the position. SessionJournal, the explicit transient `()` journal and EvaluationJournal support this.
Without installed finalizers existing implementations retain their behavior.

AgentRawEvent adds AssistantMessageFinalized {message, changed} and typed handlers gain a default
on_assistant_message_finalized. Update exhaustive matches/v1 schema consumers. The event follows
canonical acknowledgement; prior content/reasoning streaming events describe the original preview.
TUI replaces its current preview; console/Web explicitly append the final effective answer when
changed. Print/outcome/history/export use canonical effective content. This is not a stream filter.

Session versions remain unchanged: safe receipts use the inert Extension namespace
aicoder.assistant-finalization.v1. No callback, private original or authorization is restored from
these records. User/custom/tool-wide message finalization remains outside this increment.

## S7: busy custom delivery and next-turn inboxes

RunHandle adds begin_custom / submit_custom with CustomDelivery Steer/FollowUp/TurnEnd/NextTurn.
Raw user hooks and templates are bypassed; canonical custom history uses the existing typed
format. RunInput adds optional custom metadata (None for user literals); RunInputKind adds
Context for custom-only deferred writes. Update exhaustive matches and strict v1 event-schema
consumers. Old user serialization omits the new field. Event validation now checks typed pairing
and owned run identity. See [contract](custom-delivery.md).

RunJournal adds supports_custom_input (default false) and atomic input_custom. Custom journals
must explicitly implement both to accept active custom data. Session/eval journals and () support
them. Unsupported journals reject rather than recording a Custom message as an ordinary Message.
Safe delivery receipts and typed entries use one transaction; acknowledged facts/usage survive
later stops without repeating completed operations.

Agent now owns a bounded workspace/session CustomInbox. NextTurn requires a bound session and
waits for a later explicit user/custom trigger. Claims consume only after the initial transaction
acknowledges; failure/drop releases them, new arrivals remain, and handled input consumes none.
Reasoning clones and TUI reload retain the inbox. Hosts creating replacement Agents may explicitly
share custom_inbox through with_custom_inbox; another workspace rejects. Pending queues/claim
tokens never serialize or restore from history. Input IDs may have gaps after discarded futures.
Delivery and consumption provenance use inert aicoder.custom-delivery.v1 /
aicoder.custom-next-turn.v1 records; no session format version or dependency changed.

Turn-end custom context is rechecked after verification and settle waits, including before a
verification retry. A final assistant checkpoint precedes it. TUI keeps custom rows distinct from
User rows and honors display intent. No new RPC/Web client-controlled authority or route is added.
See [example](../crates/aicoder-runtime/examples/offline_custom_delivery.rs) and
[acceptance](s7-custom-delivery-acceptance.md).


## S7: sourced custom messages and idle triggers

Core adds CustomMessage, AgentPrompt and RunTrigger. Existing user entry points still accept
Into<AgentInput>; run_prompt* variants accept typed user/custom triggers. append_custom_message
performs an idle, validated append without running a model or synthesizing a turn. Custom paths
skip user raw hooks/expansion. BeforeStartInput adds optional trigger; update Rust literals.
Low-level ModelRequest histories have no inferred trigger. See [contract](custom-messages.md).

MessageRole adds Custom for canonical records only. Core projects it to a labelled compatible
User message before provider validation/counting/IO; direct unprojected requests reject. Update
exhaustive role matches and use CustomMessage::model_message only as an ephemeral provider view.
SessionEntryData adds CustomMessage and as_message includes it. Its storage remains a version 1
Extension payload under aicoder.custom-message.v1; old readers retain inert data and need upgrading
for context-equivalent continuation. The reserved namespace and ordinary Message/Custom values
cannot bypass typed validation. No session version bump or new dependency/feature was added.

CommandOutput adds SubmitCustom; update exhaustive matches or use into_prompt. Command results
adopt their registered sender before TUI/Web submission. Display/details remain domain data;
details never enter model requests or restore authority. Explicit exports retain them as inert
content. Hidden TUI entries still participate in context. Busy custom-control/next-turn delivery
remains separate work; do not represent those messages as user controls.

ExecutionInput prefix validation now preserves explicit root extension context outside a declared
host prefix, while retaining the declared prefix even when its text resembles a native label.
Run preparation carries typed trigger metadata through preflight/startup without duplicating
execution loops. Prepared snapshots and backend/approval authority remain ephemeral.
See [example](../crates/aicoder-runtime/examples/offline_custom_messages.rs) and
[acceptance](s7-custom-messages-acceptance.md).


## S7: raw input interception and ordered asynchronous controls

Extension adds input_hook and InputAction Continue/Transform/Handled. Agent prepares fresh
AgentInput before expansion/validation/persistence; raw executor histories are not reinterpreted.
AgentInput adds origin, with_prompt_text and preparation_receipt; its private prepared marker
cannot serialize or cross executor/run/delivery boundaries. Original input exists only in
memory for recovery. InputReceipt.original and new RunInput.provenance use serde skip; update
Rust RunInput literals with provenance: None. Existing control JSON stays unchanged.

Runtime now installs frozen resource expansion in the Agent input pipeline. Pass raw prompts
to Runtime-created Agents; remove previous prepare_prompt(...).text calls before Agent.run.
Otherwise an escaped literal can expand twice. prepare_prompt remains a read-only expansion /
diagnostic API. Text/image attachments after the first prompt block remain unexpanded.

Use Agent::control_channel / TurnExecutor::control_channel before publishing a handle.
begin_input reserves order synchronously and returns an owned future; submit_input is its async
convenience. Drive it alongside frontend events. Synchronous steer/follow_up reject when raw hooks
are installed. A legacy unbound channel with already queued raw inputs fails setup rather than
bypassing hooks. Queued IDs can have gaps after handled/cancelled reservations. RPC control replies
may be deferred/interleaved and retain their request IDs; correlate them by ID. TUI awaits input
acknowledgement without blocking UI, status or cancellation and keeps unsent drafts.

RunStop adds InputHandled. Initial handled input maps to AgentTurnOutcome::Stopped, without a
model answer or persisted turn; RPC/Web status adds handled and CLI succeeds. into_result/run
conveniences still require a completed model answer and return this typed stop. Update exhaustive
RunStop matches and strict v1 event/RPC schema consumers. Handled active controls return a separate
InputSubmission result and do not stop the current run. No event/session version bump is made.

RunJournal adds record_input_hooks and atomic input_prepared with default unsupported errors.
Implement both before installing raw hooks or accepting prepared control receipts. Built-in
session/eval journals and () support them. Safe receipts and effective user messages commit
atomically; private original bodies, prepared callbacks and authority never restore from history.
Whole receipts and prepared startup output share output admission, and preparation/waits retain
the original duration clock. No dependency, manifest, lockfile or default feature was added.
See [contract](input-hooks.md), [example](../crates/aicoder-runtime/examples/offline_input_hooks.rs)
and [acceptance](s7-input-hooks-acceptance.md).


## S7: once-per-run startup preparation

Extension adds before_start_hook with BeforeStartHook/Input/Action. It can replace the current
run's declared System prefix, select admitted root tools (including empty), and append bounded
inert data or sourced native context. Later handlers see preceding edits. No default hook,
settings route, dependency or default feature was added. See [contract](startup-hooks.md).

ExecutionInput adds for_run(): prepare_input(input.for_run(), context) freezes startup before
turn validation/persistence. Plain prepare_input remains a selection preview. Agent session turn
entry points opt in automatically; direct execution prepares an unprepared startup. The private
snapshot rejects another executor, cannot serialize/restore and expires with its explicit input.
It does not replace raw-input transformation or handled-input outcomes.

BoundaryKind adds BeforeStart, and BoundaryCommit adds optional startup metadata. Update exhaustive
Rust matches and direct struct literals (startup: None for ordinary boundaries). Serde defaults
and omits None, preserving older receipts byte-for-byte in value form. The optional metadata
contains actors, counts and tool names, never prompt text. Startup uses round 0 in its stored
receipt and no event round scope; the v1 event schema extends the boundary discriminator.
RunJournal's existing record_lifecycle/commit_boundary contract applies. Custom journals must
support both before installing startup; no new journal method is required. Prepared session
turns commit the original input, receipt and context in one initial transaction. Existing readers
retain opaque receipts without restoring prompt overrides or tool authority.


## S7: actionable round and settlement boundaries

Extension adds boundary_hook with BoundaryHook/BoundaryAction and typed TurnEnd/BeforeSettle
stages. They stage inert Data or explicitly sourced Context entries and may request continuation
inside the same budgeted loop. No hook is installed by default. Native registration metadata,
ordering, cancellation and scoped UI remain unchanged. See [contract](boundary-hooks.md).

RunJournal adds record_lifecycle and atomic commit_boundary, with default unsupported errors.
Custom persistent journals must implement both before installing boundary hooks. The Started
receipt probes support before provider IO; commit_boundary must persist its receipt and derived
messages together. The built-in session/eval journals implement them; () explicitly accepts
ephemeral runs. Existing journals without installed hooks need no change.

SessionEntryData adds NativeContext { source, message }; update exhaustive Rust matches or use
as_message() to read both ordinary and native messages. Use native_context(source, text) for
validated construction. Ordinary Message/System remains invalid. The existing session version
and JSONL extension shape are preserved: namespace aicoder.context-message.v1 contains version 1,
source and message. Upgraded readers validate it; older readers retain it as inert extension
data and therefore do not include that context in model views. Upgrade readers before resuming
a branch that relies on native context. Generic Extension values using the reserved namespace
now reject; do not use that namespace for unrelated application metadata. Historical receipts
never reinstall callbacks or continue runs.

AgentRawEvent adds LifecycleBoundaryCommitted and the handler adds a default no-op callback.
Update exhaustive event matches and schema consumers for lifecycle_boundary_committed in the
v1 schema. It is emitted only for explicitly installed hooks and carries metadata counts, not
private entry text. No dependencies, default features or credential routes were added.


## S7: shared Runtime and native auxiliary transport binding

Providers adds TransportBinding as a cloneable immutable callback snapshot/shared monitor.
ChatClient, SystemOneClient and OpenRouterImageClient accept with_transport_binding;
with_transport_hooks/with_shared_transport_hooks create fresh bindings. for_model clones keep
the selected binding. Replacing/clearing a binding affects newly created clients; previously
created clients keep their snapshot and diagnostic history. No default callback is installed.

Runtime adds with_model_transport_hooks/with_model_transport_binding and read-only
model_transport_catalog/report. Bind before constructing primary/session clients and default
auxiliary registries. Arbitrary externally supplied ModelProvider objects are not implicitly
replaced. Reload/resource reload/package-state adoption clone the current host binding;
RuntimeConfig/history cannot serialize/install/restore it. New loads remain unbound.

Native state/questions/images/assets/modalities aliases and the TypeSafe experimental image
opt-in header are now protected transport fields. Existing host allowlists using these names
must move semantic changes to typed preparation APIs. Native clients preserve one-attempt
semantics, zero preflight usage and reported usage before output validation. The shared driver
also preserves proxy redaction, response bounds, scoped storage and parent metering.

TransportDiagnostic adds provider/model with serde defaults for older JSON; update direct Rust
struct literals with those fields. Native client catalog/field/report getters mirror chat.
No new dependencies, package versions, event/session formats or default features changed.
See [contract](auxiliary-transport.md), [mixed-client example](../crates/aicoder-runtime/examples/offline_auxiliary_transport.rs)
and [acceptance](s7-aux-transport-acceptance.md). Actionable lifecycle and final isolated
Spark/Pi/release acceptance remain roadmap work.

## S7: compatible-client native transport hooks

Providers exports TransportHooks/TransportHook and bounded header/payload/response/parsed-stream
inputs, cancellation observation, metadata reports and the reusable TransportSession driver.
ChatClient::with_transport_hooks or with_shared_transport_hooks explicitly binds an immutable
snapshot. for_model shares it and diagnostic state; SessionModelClient therefore preserves it.
Core model traits, default builds and credential configuration remain source compatible.
Native callbacks are not loaded by settings/history or implicitly installed by core extensions;
a host can register/select the already-bound client as its provider contribution.

Request stages freeze safe headers and allowlisted service fields once before HTTP. Original
adapter fields and core-owned aliases, auth/framing/routing headers stay protected. Semantic
changes use core RequestHook before admission. Rejected preparation reports locally known zero
model usage; observers publish known stream usage before waiting and never replace normalization.
Error/panic/timeout/whole-event omissions are bounded fixed-class diagnostics, not native strings.

Opted-in clients do not retry an incomplete body after successful response headers. Configured
retryable statuses/pre-response behavior remain, using the frozen plan and fresh private bearer
credentials. Default clients without hooks preserve prior behavior. Observation data may be
redacted or omitted; it is neither exact HTTP bytes nor a billing/replay record. No dependency,
package version, event/session version or default feature changes were made.

The initial binding covers compatible chat and SessionModelClient. Existing native classifier/
image clients need explicit further integration; this and actionable lifecycle hooks remain
open. See [contract](transport-hooks.md), [loopback example](../crates/aicoder-runtime/examples/offline_transport_hooks.rs)
and [acceptance](s7-transport-hooks-acceptance.md).

## S7: provider-neutral request views

Extension adds request_hook(HookSpec, RequestHookScope, Arc<dyn RequestHook>). New typed
RequestEdit/RequestMessages own bounded message/option/tool-subset changes; identity, routing,
stream mode and definition schemas remain host controlled. Conversation scope preserves the
declared prefix; SystemAndConversation explicitly replaces it. No hooks are installed by default.
Each view validates complete tool units/capabilities and is counted before window admission.

prepare_input now freezes a private executor-bound first view when request hooks exist.
validate_model_selection requires this async preparation with installed hooks and validates that
view before session/model writes; request-only effort does not become branch selection. Applied
controls invalidate snapshots. ExecutionInput's public constructors/getters remain compatible;
its new private cache is neither serialized nor history-restored. Default paths keep borrowed
preflight measurement. Each round starts from the original host options/catalog.

Automatic compaction receives raw context/history and retains its ordinary checkpoints/usage,
then reapplies hooks to the reduced view before final validation, counting and parent clamping.
Native hooks must be read-only/idempotent across preparation/admission/post-compaction passes.
A final view still requiring compaction rejects rather than starting an unbounded retry loop.

RunJournal gains record_request_view(), rejecting by default. Custom journals must implement it
before enabling request hooks, including callbacks returning None. Unit journals explicitly
support ephemeral runs; session-v3/eval journals store inert RequestViewRecord observations.
The new aicoder.request-view.v1 namespace does not shift transcript indices or restore authority.
No session/event version, package, dependency or default feature changes are introduced.

Receipts observe effective tool names/choice, counts and safe generation scalars. Stop text,
schema names/bodies and view messages are not stored; RequestOptionSummary.complete marks
omissions. These are not full-payload fingerprints. ModelContextInfo's selected effort stays the
host preference while receipts identify effective request effort. See [contract](request-views.md),
[HTTP example](../crates/aicoder-runtime/examples/offline_request_views.rs) and
[acceptance](s7-request-views-acceptance.md). Raw HTTP/provider and actionable lifecycle stages
remain open in the Pi audit.

## S7: tool argument and delivered-result interception

Extension adds tool_call_hook and tool_result_hook registrations with existing HookSpec ordering,
limits and cancellation. New core ToolCallHook/ToolResultHook traits use owned bounded edits.
Existing ToolHook signatures remain compatible; they now see effective arguments after any
explicitly installed ToolCallHook chain. No new callbacks are installed by default.
Tool IDs/names/capabilities and owned plans remain immutable through approval/execution.

RunJournal adds record_interception(), rejecting by default. Implement it before installing
argument edits or result callbacks in a custom durable journal. Unit journals explicitly
support ephemeral SDK runs. Session v3 stores aicoder.interception.v1 Extension records; eval
journals retain the same receipts. Existing history needs no rewrite and never restores code
or authority. Argument records are proposals, while ResultPending precedes execution. Final
receipts separate actual execution/completion/capture status from delivered result edits.

Tool result envelopes may add compact interception metadata when callbacks are selected.
Original assistant arguments stay canonical, with arguments_changed and inert proposal
records marking effective changes. Result callbacks can change delivered payload/error status
and narrow existing artifact references; they cannot fabricate or restore exposed artifacts.
ToolExecutionEnded describes delivered status; inspect the immutable interception receipt for
original execution status. Redaction is of the new delivered view, not deletion of earlier
raw archives or source files. Metadata consumes parent output and complete model-view counts.

No package versions, dependencies, default features, event enum variants or session versions
changed. See [contract](tool-interception.md), [offline example](../crates/aicoder-runtime/examples/offline_tool_interception.rs)
and [acceptance](s7-tool-interception-acceptance.md). Provider/request transformations and
additional lifecycle actions remain open in the Pi audit.

## S7: native image-generation tasks

Core adds AuxiliaryModel, retaining ClassifierModel as a source-compatible type alias with
unchanged classifier JSON fields. ImageGenerationModel flattens its specification in JSON
and adds text_output; Rust hosts use its typed specification field. New ImageGenerationContext,
GeneratedBlock, ImageGenerationProvider and ImageGenerationOperation define native image
requests/raw output and immutable delivery. PreparedTool adds default
image_generation_operation() returning None. Declare exactly one native operation per plan;
ambiguous classifier/image plans reject. Both kinds share existing auxiliary admission,
usage, records and cancellation; no new persisted record or session version is introduced.

ProviderProfile adds image_models, empty when absent; Rust literals need Vec::new(). The
existing auxiliary-models build feature adds the OpenRouter native modality adapter, reusing
the already locked optional base64 package. Cargo.lock adds only that local dependency edge;
no package versions or default features change. Selected profiles may contain classifiers,
images or both, with shared route/time/response settings. Image profiles default to 120-second
HTTP and 24 MiB response limits; classifiers keep their 2 MiB response cap. The registered
model_generate_images name is reserved; add it and model_catalog to explicit leaf allowlists.

models.generateImages returns text and scoped image artifact blocks, unlike Pi's in-memory
base64 output. Use await image(block) for explicit selection, or pass the block as edit/reference
input with the optional images decoder. Do not assume an output image has data/mimeType.
SDK GeneratedBlock preserves raw bytes for host-owned storage. Storage is required before
inference; --no-session has no implicit output store. Failed staging/publishing retains known
usage and incomplete-delivery receipts without repeating completed inference. Raw response
bytes never become canonical history, while legitimate input calls/scripts are preserved.
See [contract](image-generation.md) and [acceptance](s7-image-generation-acceptance.md).

## S7: native auxiliary classifiers

Core adds the auxiliary module for typed native classifier models, questions/answers,
request-time usage reporting and immutable AuxiliaryOperation plans. PreparedTool gains a
default auxiliary_operation() hook returning None. ExecutableTool gains parallel_safe(); its
default retains the preceding read-only scheduling rule. Override it only when the host can
guarantee independent execution; it never changes capability, guards or approval requirements.

RunBudget adds optional max_auxiliary_calls. Use ..Default::default() in Rust struct literals.
BudgetResource gains AuxiliaryCalls; update exhaustive matches. RunProgress, RunTermination
and TurnStatistics add auxiliary and primary_usage with absent/empty defaults. Normal runs
keep their previous serialized shape. When tasks exist, usage includes primary and auxiliary
work and primary_usage retains the primary subtotal. Do not add these subtotals again or use
the primary model's price for every task. RPC finished results expose the same optional
breakdown. Event/RPC v1 schemas are updated; consumers rejecting new enum values need an update.

RunJournal gains record_auxiliary(), rejecting by default. Custom durable journals must store
Started before provider IO and Finished after settlement; unsupported implementations fail
as persistence errors before inference. The explicit unit journal supports nonpersistent SDK
runs. Session v3 uses inert aicoder.auxiliary.v1 Extension records, with no transcript-index
changes or restoration of execution authority. Existing history needs no rewrite.

ProviderProfile adds optional classifiers and classifier_image_input; CodeModeSettings/Policy
add model_profiles, empty by default. ProviderProfile literals need classifiers: Vec::new()
and classifier_image_input: Default::default(); CodeMode literals can use defaults. Only trusted user
profiles define native routes/credential references; project files can select existing names.
The optional runtime auxiliary-models feature exposes native tasks without JS; code-mode
includes it. Selected routes register reserved model_catalog/model_classify tools; Code Mode
also registers the catalog when empty. Resolve extension collisions and include these names
in explicit leaf allowlists. No default features, package versions or dependencies were added.

models.getModelsOfType/getAvailableOfType/getModelOfType and models.classify use typed native
System One requests, not generic chat inference. Missing usage is estimated with uncertainty;
reported usage survives invalid output. Completed tasks are not retried after failed delivery.
Native classifier/image input protocols are explicit host settings. Image generation has a
separate [contract](image-generation.md). See [classifier contract](auxiliary-models.md) and
[acceptance](s7-classifiers-acceptance.md).

## S7: Code Mode only and root tool catalogs

Core builders add model_tools(Vec<String>), model_tool_definitions() and model_tool_names().
The root catalog defaults to the complete registry; explicit empty/subset selections stay
empty/subset through preflight, counting and dispatch. Direct model calls to hidden registered
tools now return tool_not_exposed before preparation/approval, while composed leaves retain
their separate frozen catalog. Unknown tool names preserve unknown_tool. Callers intentionally
using request subsets must advertise every root tool they expect a model to call.

Runtime CodeModeSettings/Policy add only (default false); Rust literals should include
..Default::default(). CLI/TUI/Web --code-mode-only enables only exposure, and --code-mode
explicitly selects normal exposure. only requires enabled in settings/environment. No session
or authority is loaded from a past catalog. EvalProvenance adds optional model_tools when root
declarations differ from installed tools; older observations default to None. This field is
observation-only, and default serialized observations keep their old shape. No persisted
session format, dependency or default feature changes. See [contract](code-mode.md#only-mode-and-root-declarations).

## S7: explicit Code Mode image output

ProgramEvent gains ImageCall; external exhaustive Rust matches must handle it as a scoped
allowlisted read-only dispatch with explicit single-image output. Composition records/session
v3 remain unchanged. Image recovery references no longer auto-attach to new outer results;
canonical leaf records retain them. Existing historical image blocks are not rewritten.

CodeModeConfig adds an optional host-owned image_tool (default image_output). Runtime opt-in
registers that reserved tool; extension name collisions reject. Explicit Code Mode allowlists
must include it for image(). Normal tools.NAME(args) returns the same output; the additive
tools.NAME.result(args) returns a complete program-view envelope with _image_artifacts.
Await image() to select returned references or capture bounded inline data. It uses existing
leaf guards/budgets and explicit host storage. Reused base64 is added only to the optional
runtime code-mode feature; no dependency/default-feature or persisted schema changes. See
[Code Mode images](code-mode.md#explicit-images).

## S7: image MIME views

Core adds `media::image_media_type` and ArtifactRef::is_image. Image declarations now share
bounded case/parameter/alias interpretation across tool promotion, user input, model validation
and adapters. Known JPEG/BMP aliases use canonical provider headers; raw declarations and
exact stored reference identity remain unchanged. Existing session/artifact/event schemas and
dependencies are unchanged. Invalid subtype/control/non-image declarations reject at the
input/transport boundary; a malformed MCP image declaration marks incomplete media delivery
without retrying its completed operation. Unknown valid image subtypes retain an explicit
unsupported preview path. See [contract](tool-images.md#mime-declarations).

## S7: native read_file images and BMP

`ReadFileTool` keeps its public type, name, arguments and text-page/cursor format. Recognized
PNG/JPEG/GIF/WebP/BMP signatures now return a complete `kind: "image"` success with raw source
metadata and ToolSuccess.artifacts; consumers must not assume every success has text content.
Images require explicit artifact storage and default pagination. No storage/oversized sources
fail before publication; archive failures identify completed capture with incomplete delivery.

Acquisition is codec-free, including default standalone runtime/eval. Optional runtime images
adds BMP to existing user/tool normalization; no dependency or lockfile version changes.
Existing text/history and artifact/session formats remain compatible with the preceding typed
tool-image release. `--no-session` keeps its previous storage-disabled behavior. See
[native image reads](file-tools.md#native-image-files).

## S7: typed tool image artifacts

`ImageSource` gains `Artifact { reference }`; exhaustive Rust matches need a new arm.
Tool messages may now include typed image blocks beside exactly one tool result. Existing
text and user URL/base64 APIs are unchanged. Custom adapters must resolve/project tool images
or reject them explicitly; the compatible adapter never sends unresolved storage references.
Core hosts opt into `ToolImageResolver`; runtime factories install it only with optional
`images` and configured artifact storage. Non-vision models retain tool facts with explicit
preview omission; user image capability checks remain strict.

Session v3/event v1 envelopes are unchanged, but older binaries reject the new image-source
variant when reading new media-bearing records. Keep the original archive and use a newer
reader; export a separate text/reference-only copy if an older consumer is necessary. Never
rewrite an existing journal in place or restore IO authority from exported references. Old
JSON-only artifact results are not implicitly promoted or migrated. See [tool images](tool-images.md).

## S7: atomic dependency-source acquisition

PackageAcquirer::install_with_sources accepts an explicit PackageSourceCatalogue of exact
pins and host-selected addresses. Existing install uses an empty catalogue and reuses trusted
installed dependencies. Remote closure publication checks the initial registry revision and
commits once; local imports remain serialized. Only the root is enabled, preserving existing
dependency active selections. --source-catalogue and --source-token add host CLI options;
catalogues contain no credential routing. Existing session/pin/lock/registry formats remain
unchanged. Errors may retain inert admitted objects but never a partial selected graph.

## S7: remote package provenance

Optional package-downloads adds PackageAcquirer and CLI install-git/install-npm paths. Local
install remains available by default. First remote publication upgrades the private registry
to schema 2; current readers accept local schema 1 and both origins in schema 2. Older
binaries cannot read new origin variants: import verified allowlisted payloads into a fresh
older local store before downgrading. Public package locks/pins and session formats do not
change. Source URLs cannot carry credential/capability parameters; host references are not
serialized. See [transport/budget rules](remote-packages.md).

## S7: explicit package palette data

`Runtime::package_theme_asset` reads only declared installed themes under host policy and
returns an immutable `PackageAsset` with pin/path/bytes getters. TUI --theme-package and
/theme PACKAGE:PATH use the existing 4 KiB palette format. Automatic resource discovery and
global enablement are not required for an explicit selection; package-host disabling still
applies. Reading data grants no executable authority. Existing palette/file options and
session/package formats remain compatible; invalid selections preserve the current colors.

## S7: explicit package executable owners

CLI/RPC/TUI/Web opt into installed package executables with --activate-packages and an
optional --package-programs host file. Runtime::activate_host_extensions preflights selected
package trust/profiles with direct stdio settings and returns one owner/catalog. Existing
activate_extensions and PackageSnapshot::activate APIs remain available. Installation and
resource reload do not start code; executable changes require an explicit new generation.

PackageActivation drop now invalidates retained tool clones and signals cleanup independently
of Tokio. Await shutdown for its cleanup report. Package extension catalog versions now have
the opaque form package-sha256:DIGEST@PEER_STAMP; tool names/wire protocols are unchanged.
They identify package content and observed peer metadata, not automatic host binary identity.
ActivatedExtensions::finish preserves a run error together with cleanup uncertainty. No
persisted session/package format changed and execution grants are not restored from history.

## S7: source observations for isolated benchmarks

`SourceStamp::load_observation` accepts a bounded explicit host observation only when current
executable, platform and compiler match. `aicoder-bench observe-source` emits that observation;
`run --source-observation` permits execution without the source checkout. Source contents
remain the host's claim and grant no authority. Existing runs still capture directly and
keep their original parameter shape; persisted benchmark/session schemas are unchanged.
The optional Pi comparison now requires Linux Bubblewrap and freezes full executable
packages instead of running tools on the host. Historical unisolated evidence remains valid
only under its recorded exploratory limitations; it is not relabelled by a newer reader.

## S7: host extension adoption

`Runtime::with_extensions` explicitly adopts an immutable registration snapshot; reloads
preserve it. Builders now include that snapshot. `executor_builder_with_extensions` combines
additional native registrations, rejecting collisions instead of replacing host-installed
tools. Hosts without an adopted snapshot keep their existing behavior. The separate
`ActivatedExtensions` guard owns configured process/MCP connections and must be retained
and shut down explicitly; cloning runtime registrations never extends process authority.
See [host launch files](host-extensions.md). No persisted session format changed.

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
