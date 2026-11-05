# Once-per-run startup hooks

`Extension::before_start_hook(HookSpec, Arc<dyn BeforeStartHook>)` installs an explicit
host-owned startup stage. It runs after ordinary resource/context preparation and before
request-local hooks, counting, compaction and the first model call. No callback is installed
by default. CLI/TUI/Web/bench hosts use the same core/runtime executor.

`BeforeStartInput` borrows the current request, canonical history, declared runtime-prefix
length, staged entries and immutable available root tools. `BeforeStartAction` can:

- Replace this run's runtime system prefix with validated System messages. Recorded user,
  assistant, tool and explicitly sourced context messages stay intact.
- Select a unique subset of the initial host-admitted root catalog, including an empty set.
  A later handler may reselect an initial root. It cannot add a new tool, change schemas or
  expose Code Mode leaf tools. Actual dispatch still enforces offered roots and leaf approvals.
- Append inert `BoundaryEntry::Data` or explicitly sourced `BoundaryEntry::Context` messages,
  using the same [canonical source/wire representation](boundary-hooks.md) as round boundaries.

Handlers use native HookSpec priority/order, timeout and scoped cancellation. Later handlers
see the modified prefix/tool selection and earlier proposed messages. Tool selection lasts for
all rounds of this run; per-request hooks may narrow it further. An empty selection stays empty
through preflight and execution. Prefix overrides expire between turns, including on the same
Agent. They do not edit settings, credentials, provider/model routing or approval authority.

## Preparation and persistence

Agent session turns freeze startup before model/context compatibility validation and journal
mutation. The original user message, startup receipt and native context entries commit together
in the initial turn batch. Failure/cancellation before that write leaves the existing session
unchanged. A new-session request can already have created its empty session container.

`ExecutionInput::for_run()` opts an SDK preparation into startup. Use
`prepare_input(input.for_run(), context)` to freeze the stage before inspecting/selecting a turn.
A plain `prepare_input` remains a model-selection preview and does not run startup. Actual
execution runs the stage if it was not prepared. Prepared values are immutable, executor-bound
and ephemeral; another executor rejects them. Repeated preparation and first-request admission
do not call startup again. Controls accepted during preparation join the owned run without
restarting startup. Request views are rebuilt if a queued control invalidates their preview.

Direct executor calls use the existing `RunJournal` lifecycle/atomic boundary contract.
Unsupported journals reject before startup callbacks or provider IO. In a prepared session turn,
startup context is already committed in the initial batch. Startup output then charges the same
run budget once; later persistence failures never cause callback/model/tool retries. An
acknowledged direct commit is reflected in progress before checking elapsed limits again.

Receipts use `LifecycleRecord::Applied`, `BoundaryKind::BeforeStart`, round 0 and optional
`BoundaryCommit::startup` metadata. Metadata records actor changes, prefix message count and
selected tool names; it omits system-prompt text. Explicit appended context is deliberately
recorded as a sourced message. Inert Data is stored as metadata and never becomes prompt text.
The `lifecycle_boundary_committed` notification has no round scope and carries counts only.

Each changed prefix/tool-name list is bounded to 64 KiB; staged entries have the existing
32-entry/64-KiB aggregate and 32-KiB body limits. The complete edit chain is bounded to 256 KiB,
its receipt to 128 KiB, and receipt/messages/changed-prefix output to 256 KiB. Complete output
is admitted against the run output budget before a session write. The final effective request
is counted against known token/context windows. Per-hook and whole-phase timeouts, cancellation,
error/panic sanitation and private-child cleanup use the native extension contract.

Restoration retains sourced context and inert receipts. It never reinstalls hooks, private
prefixes, active-tool selections, continuation, provider routes or grants. Branches, semantic
summaries, bounded archives and inert exports use actual native message entries; archive
metadata descriptors remain outside model context.

Run the [offline runtime example](../crates/aicoder-runtime/examples/offline_startup_hooks.rs):

```sh
cargo run --locked -p aicoder-runtime --example offline_startup_hooks
cargo run --locked -p aicoder-runtime --example offline_startup_hooks -- --deny
cargo run --locked -p aicoder-runtime --features code-mode --example offline_startup_hooks -- --code-mode
cargo run --locked -p aicoder-runtime --features code-mode --example offline_startup_hooks -- --code-mode --deny
```

It uses real runtime tools, approvals, JSONL, archive windows and HTML export with a scripted
provider and no credentials/network. See [acceptance](s7-startup-hooks-acceptance.md) and
[migration](migrations.md). [Raw input hooks](input-hooks.md) separately implement the
continue/transform/handled gate before this stage. [Idle custom-message submission](custom-messages.md) now provides typed trigger metadata.
Message transforms, session navigation veto and boundary
context-edit/compaction drafts remain open in the [audit](pi-parity-audit.md).
