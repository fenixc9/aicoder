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
| Extension tools/hooks/commands/UI | Immutable native registration; process/MCP tool adapters; [explicit stock launch](host-extensions.md); TUI/Web native UI examples; [S6 acceptance](s6-ui-acceptance.md) | Partial: supported tool/request/HTTP drivers and native bindings verified; actionable lifecycle remains |
| Resource package discovery/install/pins/reload | Content-addressed local imports, locks/trust/dependencies and public branch state; [packages](packages.md) | Verified scoped Git HTTPS/npm native closures and activation; no JS/TS ABI claim |
| Themes, export/share | TUI themes and inert canonical JSON/HTML exports, explicit publication adapter; [exports](session-exports.md) | Verified scoped local/custom-host path; no third-party hosted sharing deployment |
| MCP and Code Mode | Actual modern/legacy stdio, [stock stdio hosts](host-extensions.md), real JS composition with leaf approvals/budgets/journal, CLI opt-in; [Code Mode](code-mode.md), [MCP](mcp.md) | Verified scoped MCP, JS and native auxiliary models; protocol/limit differences remain |
| Multimodal | Immutable bounded local CLI/TUI image input and [typed tool/MCP previews](tool-images.md); [tool image acceptance](s7-tool-images-acceptance.md) | Verified scoped native/MCP image paths and image generation; no real vision-quality score |

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
   longer auto-expose filtered pixels in new runs. Native typed classifiers now have a host catalog, native System One adapter, immutable task
   plans, parent reservations and per-attempt usage through SDK/Code Mode; see
   [classifier acceptance](s7-classifiers-acceptance.md). Native image generation now uses a distinct
   OpenRouter modality adapter through the same meter, with text/reference inputs, staged raw
   artifacts and explicit image selection; see [image generation acceptance](s7-image-generation-acceptance.md).
   Generated script images use scoped artifact blocks instead of large base64 replies.
   Normal mode keeps ordinary root declarations; only mode now declares codemode alone while
   preserving the independently allowed leaf registry. Fresh undeclared direct calls reject
   before preparation, with paired canonical failure records and actual root catalog bench
   provenance. See [only-mode acceptance](s7-code-only-acceptance.md). Different source syntax,
   smaller host limits and TypeScript declarations versus JSON schemas are explicit protocol
   differences, not by themselves proof of missing underlying execution capability.
5. **Interception breadth.** Native ContextHook appends bounded prefix text; ToolHook gates
   before preparation. Pi's [extension contract](https://github.com/earendil-works/pi/blob/4a080252f06caf63a525d7873c5d94d3c23887e0/packages/coding-agent/docs/extensions.md)
   offers additional transport/lifecycle stages. Native tool arguments and delivered results now
   have ordered typed transformations, owned approval, persistence receipts, immutable execution
   status and narrowed artifact exposure; see [tool interception acceptance](s7-tool-interception-acceptance.md).
   Provider-neutral request views now implement conversation/full-prefix transforms, request-local
   options and tool subsets before counting/admission, with frozen previews, raw-summary rebuilds
   and actual JSON/SSE adapter checks; see [request-view acceptance](s7-request-views-acceptance.md).
   Compatible chat clients now bind ordered safe headers/allowlisted service payload controls,
   response metadata and parsed-stream observers with bounded diagnostics, usage-before-observer
   publication and unchanged normalization; see [transport acceptance](s7-transport-hooks-acceptance.md).
   Core semantic fields remain in counted request views; transport callbacks cannot alter them
   after admission. Explicit immutable Runtime bindings now reach native classifier/image and
   created primary clients, survive reload, preserve approval/usage, and share bounded diagnostics;
   see [native transport acceptance](s7-aux-transport-acceptance.md). Arbitrary new provider drivers
   still require explicit integration. [Actionable round/settlement boundaries](boundary-hooks.md)
   now stage inert data, source-labelled canonical context and continuation in the same metered
   loop, with atomic JSONL and queue rechecks. Context-edit/compaction boundary drafts remain open.
   [Assistant finalization](assistant-finalization.md) now provides ordered text/visible-reasoning
   edits after paid provider completion, atomically persisting the effective message before tool
   dispatch and verification. Role, action blocks and opaque provider state remain protected;
   raw streaming previews remain separate. [General message finalization](message-finalization.md)
   now covers fresh User/Custom/Tool and sourced System context, including raw/expanded input,
   idle/queued/next-turn delivery and startup/boundary transactions. Actual tool status/capture/media
   and custom/context provenance remain protected; specialized hooks handle status/argument edits.
   [Acceptance](s7-message-finalization-acceptance.md) includes real owned tools, Code Mode, PTY and
   authenticated SSE. Existing histories are not re-finalized; benchmark hosts needing fresh-input
   hooks use the same AgentPrompt APIs. Native failure policy/limits and TypeScript ABI still differ.
   [Startup hooks](startup-hooks.md) now freeze run-level System prefix replacement, admitted root
   selection and sourced context before turn validation/persistence. Empty catalogs, once-only
   preparation, queueing while busy, restored context and expiring private overrides are verified.
   [Idle custom messages](custom-messages.md) now support typed source/text/images/display/details,
   record-only and trigger-turn session paths, explicit startup trigger metadata, and native
   TUI/Web command submission without synthetic users. Canonical custom history stays separate
   from JSON/SSE provider projections. [Busy custom delivery](custom-delivery.md) now supports
   steer/follow-up, safe no-trigger context and bounded session next-turn inboxes. Claims, late
   arrivals, cancellation, failed preparation, explicit generation transfer and verification
   retries have separate executable evidence; stored metadata never restores queue authority.
   [Raw input interception](input-hooks.md) now implements ordered continue/transform/handled
   before expansion or steering notification. Explicit handled outcomes, ephemeral raw recovery,
   nonblocking RPC/TUI submission and authenticated Web initial input are exercised separately;
   the stock Web example still exposes cancellation/approvals rather than steer/follow-up routes.
   [Session lifecycle hooks](session-lifecycle.md) now guard owned navigation and explicit startup /
   new/resume adoption, with start/shutdown/reload observations in SDK/CLI/RPC/TUI/Web. Plans fix
   targets/transactions under leases; window previews retain real history, model compatibility and
   source conflicts. Rejected changes retain the binding; post-commit callback failures retain facts.
   [Independent session fork/clone](session-fork.md) now covers before/at/current ancestry,
   frozen identities, summary/context/tool facts, bounded cold archives and atomic no-clobber
   publication. SDK/CLI/RPC/TUI/Web and browser draft-only restoration have executable evidence;
   visible publication failures retain the target identity without repeating work. The pinned
   runtime's before-fork handler consumes cancellation; skipConversationRestore exists in its
   declaration but is not consumed by that fork path. Tree summary customization, compaction
   replacement and context-edit/compaction drafts remain open. [Persistent idle context edits](context-edits.md)
   now append ID-bound Text/Blocks/Omit/Restore records through owned, model-validated session
   plans. Full snapshots and bounded cold archives retain raw history and whole tool units.
   Projection-aware semantic summary preparation remains open; no boundary draft equivalence is claimed.
   Provider wrappers can cover some
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

The next implementation items are tree-summary and compaction interception, and boundary
context-edit/compaction drafts. The final audit must rerun the matrix
against actual executable paths and list any user-approved scope exceptions explicitly.

[Terminal/performance follow-up](s7-terminal-exit-followup.md) records the TUI terminal-owner
exit window and matched smoke sampling. It supplements the retained footprint observations;
no real-model comparison or release-quality gate is closed by those measurements.
