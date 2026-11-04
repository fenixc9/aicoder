# Optional Code Mode (S6)

Code Mode composes registered tools from JavaScript in an isolated QuickJS runtime. The engine
lives in a separate optional crate; default core/runtime/frontend builds do not acquire a JS
engine. The pinned Pi comparison uses its [v1.4.0 codemode contract](https://github.com/earendil-works/pi/blob/4a080252f06caf63a525d7873c5d94d3c23887e0/packages/coding-agent/docs/codemode.md).
This implementation uses the Rust/JSON tool protocol, not Pi's TypeScript plugin ABI.

The core composition driver accepts tool intents from a bounded program interface. Every
leaf goes through the same frozen registry, guards, read-only preparation, approval/cache,
owned-plan commit, cancellation, output archive/limits and run budget as an ordinary tool.
Script permission never grants a leaf permission. Host allowlists cannot be changed by code,
stored values, tool annotations or restored history. Composite recursion is rejected.

Leaf calls/results are real canonical composition extension records, separate from the outer
assistant/tool pair and its provider projection. Write an intent before invoking an operation;
write completed results once. Persistence failure stops admission, never retries an operation
and never publishes an unfinished call as complete. Root transcript indices remain canonical.
Existing tool events use distinct leaf call IDs under the same run; composition records carry
explicit parent/scope/sequence linkage. Explicit script output supplies the next model
text, alongside bounded outer composition/recovery metadata.

Read-only calls may run concurrently; mutation/command calls serialize after earlier work,
retaining normal approval semantics. Budget admission is atomic across queued leaf calls and
charges the parent run. Unawaited calls are cancelled when a script ends; completed effects
are not undone. Cancellation/timeout/steering closes the program, expires its intents and
awaits bounded engine cleanup. Dropping a future still signals stop independently of Tokio.

The VM exposes tools, text/console/return, explicit image output, typed classifier helpers,
discovery and bounded store/load. It has no Node,
filesystem, network, timers, module loader, credential lookup or direct model-service client.
Memory, stack, interrupt checks, wall time, calls, queues, JSON and output have explicit limits.
Store data is branch-local public JSON committed only on script success; it cannot restore
live tool/backend/authorization state. Cleanup or archive failures do not replay completed work.

## Enable and use

Build the chosen host with `code-mode`, then opt in at startup. The CLI, TUI and Web flags
all select runtime settings; they do not contain another agent execution loop:

```sh
cargo run --locked -p aicoder-cli --features code-mode -- --code-mode --print 'inspect the project'
cargo run --locked -p aicoder-tui --features code-mode -- --code-mode
cargo run --locked -p aicoder-web --features code-mode -- --code-mode --help
cargo run --locked -p aicoder-runtime --features code-mode --example offline_code_mode
```

A settings-v1 file can contain:

```json
{"version":1,"settings":{"code_mode":{"enabled":true,"tools":["read_file"]}}}
```

Choose actual names in the installed registry. Omit `tools` (or set it to null) to offer all
non-composite tools in the frozen registry, including explicit native/process/MCP extensions.
An empty list offers none. This selects the program catalog only. In normal mode, ordinary
root tools remain declared under their existing policies. Root exposure is selected separately
with the only setting below; neither setting grants leaf permissions or a read-only profile.
Absent tools, recursive composites, duplicate names and alias
collisions fail closed before a program starts. `AICODER_CODE_MODE=true` and
`AICODER_CODE_MODE_TOOLS='["read_file"]'` provide environment equivalents; normal field-level
settings precedence applies. Engine-free builds reject an enabled setting before model IO.
The default is disabled, including when the engine feature is compiled.

SDK hosts can register `aicoder_codemode::CodeModeTool::new(CodeModeConfig::default())?`
using the crate's `engine` feature, optionally calling `only_tools` first. Other bounded
engines can implement core's `ToolCompositionFactory` / `ToolProgram` without depending on
QuickJS. A composite cannot also supply an owned PreparedTool: the dispatcher rejects that
combination before approval rather than discarding an approved operation.

## Only mode and root declarations

`--code-mode-only` on CLI/TUI/Web enables Code Mode and declares only codemode to the outer
model. It conflicts with --code-mode, which explicitly selects normal exposure. RPC inherits
its CLI host setting. The same settings-v1 configuration is:

```json
{"version":1,"settings":{"code_mode":{"enabled":true,"only":true,"tools":["read_file","image_output"]}}}
```

Environment equivalents are `AICODER_CODE_MODE=true` and `AICODER_CODE_MODE_ONLY=true`.
Each field uses normal settings precedence. only requires enabled; a contradictory configuration
rejects before model IO. Default only is false. Script discovery/calls still use the separate
frozen leaf subset, including extensions and explicit images, with all guards/approvals/budgets.

Core SDK hosts can choose any bounded root catalog with
`TurnExecutorBuilder::model_tools(Vec<String>)`. None offers all registered tools; an empty
selection offers none. Names must be unique installed entries (at most 256, bounded lengths),
validated after extension installation. model_tool_definitions() reports the selected catalog;
tool_registry() still reports the full host registry. Explicit request subsets must match that
host selection. Preparation, model selection, context/token measurement and manual compaction
use the same declarations before journal mutation. Old canonical calls/results remain history,
not newly granted ability to invoke their names.

Fresh model calls to a registered but undeclared root tool return a paired tool_not_exposed
error with operation_completed false, before guards, preparation or approval-cache lookup.
Unknown names retain unknown_tool. Attempts still use normal parent budgets and canonical
journaling. Composed leaves use their factory's separate catalog and dispatcher path; root
exposure does not secretly remove them. A model ignoring the advertised schema cannot bypass
only by returning a direct write_file call. This strengthens explicit request subsets in normal
mode too. ToolChoice restrictions remain independently enforced.

Benchmark provenance keeps the installed tools and optionally records narrower model_tools.
This describes requested effective root declarations; it is observation data, never a rule
restored into replay execution. Old observations deserialize with the optional field absent.
See [only-mode acceptance](s7-code-only-acceptance.md).

The model calls `codemode` with one JSON field, `code`, containing an async function body:

```js
const results = await Promise.allSettled([
  tools.read_file({path: "src/lib.rs"}),
  tools.read_file({path: "Cargo.toml"})
]);
text(results.filter(r => r.status === "fulfilled")
  .map(r => r.value.content));
store("reads", (load("reads") || 0) + 2);
```

Successful tool promises resolve to the tool envelope's output; failures reject a data-only
ToolError with name/message/code/details. `tools["exact.name"]` uses canonical names;
`tools.exact_name` is the identifier alias. Discovery returns both name and alias. `ALL_TOOLS`
contains bounded descriptions; async `searchTools(query, {namespace, limit})` searches those
descriptions, `describeTool(name)` supplies its schema and `describeNamespace(prefix)` lists
`prefix__` names. Discovery and script values never expand the host allowlist.

`text(value)`, console methods and a non-undefined return become separate JSON output items
or console entries. Output is buffered until completion/error. `exit()` ends the script successfully. Exceptions preserve bounded
partial output in the outer tool error. These explicit outputs and normal outer
composition/recovery metadata enter the next provider projection, while canonical leaf records
remain available to storage/export/bench. Image recovery references stay with canonical leaf
facts; only explicit image output is attached to the outer result. Non-image recovery archives
retain their existing outer references. Do not use the public store for credentials.
`store(key, value)` saves a JSON copy; an omitted/undefined value deletes a key; `load` returns
a fresh copy or undefined. The store follows the selected branch and survives cold JSONL
reopen. Transient runs without a repository have no durable store.

## Explicit images

Use the complete tool envelope when selecting media. Normal tool promises still resolve to
unchanged output values; `.result(args)` executes the same leaf once and resolves its complete
host envelope, including artifact references. Errors still reject ToolError. The program-only
`_image_artifacts` field identifies image candidates without changing the canonical envelope.

```js
const capture = await tools.read_file.result({path: "screenshot.png"});
await image(capture);
text("Selected screenshot");
```

`await image(value)` accepts a full result envelope, an ArtifactRef, a MCP image block with
an `artifact`, `{artifact_id}`, an array of sources, or inline `{data, mimeType}` / `{data,
media_type}` base64. Every single-image output becomes a core ImageCall to the configured
host tool. Runtime registers read-only `image_output` when Code Mode is enabled; SDK hosts
can install `aicoder_runtime::tools::ImageOutputTool` or configure a different immutable
`CodeModeConfig.image_tool`. Core installs no implicit tools and a missing/omitted host entry
fails before execution. Explicit tools subsets must include image_output to use image().

Image calls use the same frozen allowlist, guards, dispatcher, parent budgets and cancellation
as ordinary leaves. The image marker rejects tools whose host capability is not ReadOnly;
it cannot smuggle a mutation through an image operation. An image result must provide exactly
one valid host image reference. Up to four image intents per program are admitted; their
references retain intent order even when read-only operations complete concurrently. Completed
selected images survive a later script exception as partial output. Unfinished calls remain
abandoned/cancelled, and failed capture does not repeat completed effects or commit late state.

Inline data is archived as immutable raw bytes; an artifact ID is looked up only in the
current explicitly installed store. Supplied metadata/history/state cannot choose a store,
route or path, restore execution grants or fetch URLs. Reference selection requests a bounded
page to revalidate current host metadata; backend integrity checks retain their existing
storage bounds. ArtifactRef.complete describes source capture, separately from selection and
model preview delivery. Runtime image normalization retains the [tool image contract](tool-images.md).
Non-vision models and codec-free hosts receive explicit preview omission. No-session retains
its existing storage-disabled behavior; custom SDK hosts can explicitly choose a memory store.

An image call consumes a leaf slot and the parent tool-call quota. The default 64 KiB argument
limit applies to inline base64 too (about 48 KiB raw before JSON overhead); larger media should
use already captured artifact IDs. The host image output operation also admits at most 8 MiB
raw bytes, bounded by storage, and uses a 250 ms storage sub-deadline. Root program output and
resolved image previews retain separate existing budgets. Calls must be awaited for reliable
completion; merely calling an async image helper does not guarantee completed delivery.

Reading an image alone no longer sends its pixels to the next model. Canonical leaf results
retain the raw image reference; explicit image intents select outer image blocks. This applies
to new executions. Existing historical outer image blocks remain canonical data and are not
rewritten. Raw literals intentionally embedded in JS/arguments remain code/input text; binary
image artifacts and normalized previews are not counted as base64 text tokens.

## Bounds and accounting

| Host default | Limit |
| --- | --- |
| Source / arguments per leaf | 64 KiB each |
| VM heap / JS stack | 8 MiB / 256 KiB |
| Live VMs per tool registration | 4; excess fails immediately |
| Native owner thread stack | 2 MiB reservation per active VM |
| Program wall time | 30 s, further bounded by parent tool/run timeouts |
| Interrupt checks / microtasks | 100,000 / 20,000 |
| Leaf intents / parallel reads | 32 / 4 |
| Leaf result payload | 64 KiB, further bounded by parent output policy |
| Explicit output / items | 32 KiB / 128 |
| Public store | 64 KiB, 64 keys of at most 64 bytes |
| Frozen catalog | 256 tools / 512 KiB metadata |
| Intent queue / reply queue | 64 entries each |
| Independent engine shutdown | 5 s |

Trusted SDK hosts can adjust validated CodeModeConfig/CompositionLimits within hard bounds;
model input cannot raise them. Parent tool-call accounting includes the outer call plus every
admitted leaf. Parent output accounting includes leaf results even when the script filters
them away; filtering saves subsequent model context, not execution budget. Provider usage
continues to charge actual model requests. The composition engine creates no independent run budget. Native classifier helpers emit
ordinary metered host-tool calls; their native usage/reservations count toward the same parent
run. The image helper emits provided/captured media through existing tools. The existing calls-per-round setting bounds the provider's outer batch and also caps
parallel leaf execution; sequential leaf totals use the separate program/root call limits.

Atomic admission covers ordinary siblings in the same batch: a program cannot spend the
remaining root quota and then let a later mutation escape it. Read-only status comes from
trusted host registrations, never JS/schema/MCP annotations. Command tools, including
scripts, retain conservative approval and execute serialization.

Composition facts use the existing session-v3 Extension record with namespace
`aicoder.composition.v1`; the branch store uses `aicoder.codemode.store.v1`. Started/Call/Result/
Finished include scope, parent call or sequence and abandoned intents. A result is durable
only after its record succeeds. Unknown completion remains abandoned, not a synthesized
successful tool message. VM exceptions, steering and cancellation before commit do not save new store state.
After a successful VM finishes, its store commit precedes final journal/output delivery; a
later delivery/archive failure cannot roll back that durable state or completed effects. Backends implementing RunJournal must explicitly
support composition recording; the default rejects this optional feature. Old readers retain
these unknown extension records without projecting their payload as model instructions.

## Pi comparison and limits

This covers the S6 bounded JavaScript composition item: actual promises, loops, parallel
reads, filtering, discovery, explicit output and branch state over existing tools. The pinned
Native classifier catalog/execution helpers now use [shared auxiliary task metering](auxiliary-models.md).
Pi also describes image-model generation, source pragma/options syntax and TypeScript extension
ABI; those remain outside the implemented paths listed here. Call available media tools through their normal result/artifact contracts.
This is functional coverage, not evidence of equal coding success or performance. S7 retains
the full matrix review and comparison under identical models/tasks/budgets.

QuickJS has no direct OS bridge in this embedding. Exposed host tools still execute with
their host's permissions; their guarantees depend on their actual implementations. VM
memory bounds are not a bound on the entire Rust process or an OS sandbox for Bash.
