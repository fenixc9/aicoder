# Pi parity audit in progress (S7)

This audit uses Pi v1.4.0 / `4a080252f06caf63a525d7873c5d94d3c23887e0`, the existing roadmap
comparison pin. The locally installed Pi package reports 1.4.0; Node is 22.19.0. This confirms
an available comparison executable, not its task performance or every build input. aicoder's
current evidence comes from the acceptance records below, each with its frozen source and
exact checks. A green row is limited to the named roadmap capability and verified Linux path.
It does not certify all Pi convenience features or arbitrary third-party extensions.

## Roadmap section 4

| Capability | Current evidence | Audit status |
| --- | --- | --- |
| SDK, streaming events, cancellation | Agent/TurnExecutor/RunHandle; controlled_agent and event_stream examples, core run_control/event_stream tests; [events](events.md) | Verified within named scope |
| Custom provider/model and session switching | ModelProvider, declared ModelSpec, SessionModelClient, checked branch selection; [model API](model-api.md), session_models/configuration tests | Verified; provider breadth intentionally limited |
| Thinking controls | ReasoningEffort, capability/adapter checks before journal mutation, CLI/TUI controls and real PTY tests; [acceptance](s5-reasoning-acceptance.md) | Verified within named scope |
| Read/write/edit/search/shell | Actual runtime tools with owned preparation, bounded pages/capture, conflict/cancel paths; [S3 acceptance](s3-approvals-acceptance.md) | Verified within named scope |
| Project instructions, system prompt, skills/templates | Frozen resource generations, nested instructions, allowlisted assets, provenance and actual resource tests; [resources](project-resources.md) | Verified within named scope |
| Automatic/manual compaction | Bounded tool-free summaries, durable branch checkpoints, usage/budget charging and recovery; [acceptance](s4-summary-acceptance.md) | Verified within named scope |
| Tree sessions, branches/labels/recovery | Canonical v3 branches, bounded archive windows, navigation leases, opaque checkpoints; [sessions](sessions.md) | Verified within named scope |
| Steer and queued follow-up | Root controls/approvals, queue limits, current mutation ownership and skipped remaining calls; run_control and terminal tests | Verified within named scope |
| Interactive, print/JSON, RPC, SDK | Shared core/runtime with real PTY/browser and normalized SDK/CLI/RPC/Web case; [S5 acceptance](s5-images-acceptance.md) | Verified basic flows; no shortcut/ABI equivalence claim |
| Extension tools/hooks/commands/UI | Immutable native registration; process/MCP tool adapters; [explicit stock launch](host-extensions.md); TUI/Web native UI examples; [S6 acceptance](s6-ui-acceptance.md) | Partial: stock process/MCP/package activation verified; hook breadth remains |
| Resource package discovery/install/pins/reload | Content-addressed local imports, locks/trust/dependencies and public branch state; [packages](packages.md) | Verified scoped Git HTTPS/npm native closures and activation; no JS/TS ABI claim |
| Themes, export/share | TUI themes and inert canonical JSON/HTML exports, explicit publication adapter; [exports](session-exports.md) | Verified scoped local/custom-host path; no third-party hosted sharing deployment |
| MCP and Code Mode | Actual modern/legacy stdio, [stock stdio hosts](host-extensions.md), real JS composition with leaf approvals/budgets/journal, CLI opt-in; [Code Mode](code-mode.md), [MCP](mcp.md) | Partial: stock MCP works; additional Code Mode capabilities remain |
| Multimodal | Immutable bounded local CLI/TUI image input and [typed tool/MCP previews](tool-images.md); [tool image acceptance](s7-tool-images-acceptance.md) | Partial: native/MCP image paths including BMP verified; MIME compatibility verified; Code Mode auxiliary models/only mode remain below |

## Confirmed remaining differences

The following are functional differences, not implementations hidden behind an unchecked API.
They remain work toward the original goal; the S0–S6 implementation checklist is not evidence
that full product parity has already been achieved.

1. **Stock package activation is now verified.** CLI/RPC/TUI/Web adopt installed process/MCP
   entries through explicit flags with trust/approval/cleanup checks. TUI startup and /theme
   select declared package palettes as bounded verified data, independently of executable
   activation. Resource reload retains the current executable generation and captured colors;
   explicit restart/selection adopts changes. These paths are no longer outstanding gaps.
2. **Remote native dependency acquisition is now verified.** Explicit host catalogues map
   exact dependency pins to Git HTTPS/npm requests, recursively fetch missing content, reuse
   installed trust and publish the validated closure once. Missing/mismatched sources,
   aggregate budgets, cancellation and concurrent edits preserve the old selected state.
   Only the root is enabled and existing active dependency selections stay intact. SSH and
   npm JavaScript/TypeScript ABI resolution remain protocol/distribution constraints.
   See [remote packages](remote-packages.md).
3. **Tool image delivery is now verified.** Canonical tool results retain immutable typed
   artifact references. Explicit scoped runtime resolution validates complete source bytes,
   codecs and budgets; the compatible adapter places image parts after all matching results.
   CLI/RPC/Web/TUI and actual MCP-to-HTTP/session-replay checks pass. Non-vision, malformed,
   missing, partial and excessive previews remain explicit without repeating tool operations.
   Core adds no codecs or implicit storage; standalone Web opts into images. Existing JSON-only
   historical references stay reference-only, and this is not a visual reasoning quality score.
   See [tool image acceptance](s7-tool-images-acceptance.md). Native read_file now recognizes
   all five supported formats by signature, including BMP, and captures immutable raw artifacts
   through the same opened descriptor. Text paging/cursors remain compatible, and source/path,
   output/storage budgets and cancellation are checked independently of preview decoding.
   Codec-free runtime acquisition plus actual CLI/RPC/Web/TUI image reads are verified in
   [native image acceptance](s7-native-images-acceptance.md). Optional user/tool normalization
   also supports BMP; image previews remain static and bounded. MIME case,
   parameters and JPEG/BMP aliases now share a bounded interpretation across core/runtime/
   adapter paths, while immutable raw declarations and actual codec checks remain intact.
   Actual MIME-alias MCP/HTTP/replay and four-stock-host checks pass; see
   [MIME acceptance](s7-mime-acceptance.md).
4. **Code Mode auxiliary models and output.** Real JS, discovery/filtering and branch state
   are implemented. Pi additionally exposes image output and non-chat classifier/image-model
   helpers through its pinned [Code Mode contract](https://github.com/earendil-works/pi/blob/4a080252f06caf63a525d7873c5d94d3c23887e0/packages/coding-agent/docs/codemode.md).
   Explicit image output is now implemented through the ordinary scoped tool dispatcher,
   with inline/reference input, raw leaf history and selected model views; see
   [Code Mode image acceptance](s7-code-images-acceptance.md). Recovery image references no
   longer auto-expose filtered pixels in new runs. Classifier/image-model helpers remain absent.
   Pi's default on mode keeps normal tools declared, like the current
   aicoder behavior; its additional only mode is not implemented. Different source syntax,
   smaller host limits and TypeScript declarations versus JSON schemas are explicit protocol
   differences, not by themselves proof of missing underlying execution capability.
5. **Interception breadth.** Native ContextHook appends bounded prefix text; ToolHook gates
   before preparation. Pi's [extension contract](https://github.com/earendil-works/pi/blob/4a080252f06caf63a525d7873c5d94d3c23887e0/packages/coding-agent/docs/extensions.md)
   offers more lifecycle/request/result transformations. Provider/tool wrappers can cover some
   host integrations, but no exhaustive executable equivalence check exists. Observation-only
   events must not quietly become mutation channels or rewrite canonical facts.

Broader provider/OAuth catalogs, a built-in local-model router and identical TypeScript/plugin
wire formats are not the roadmap's initial provider/ABI gate. Missing functional paths above
are not waived by those explicit exceptions. A smaller API surface counts as equivalent only
when an actual host integration demonstrates the same requested capability.

## Evaluation and release still open

The [coding archive](s7-bench-acceptance.md) and
[pinned official SWE bridge](s7-swe-acceptance.md) establish contracts, artifact identity and
full failure denominators. [The first real Spark/Pi smoke](pi-comparison.md) supplies four
same-task/model cases per agent with shared request/tool/output limits: aicoder completed and
passed 3/4, Pi 4/4; both patches passed hidden tests 4/4. Non-sandboxed shared /tmp effects and
incomplete Pi package pinning keep this exploratory, not final comparison acceptance. The
original score/release requirements stay open; local scripted scores cannot substitute for them.

The next implementation order is Code Mode auxiliary models and only mode, then interception
coverage. The final audit must rerun the matrix
against actual executable paths and list any user-approved scope exceptions explicitly.
