# Project Instructions

## Architecture

- `crates/aicoder-core` owns the reusable agent loop, model protocol, tool/policy contracts,
  events, cancellation, session domain and memory storage. Keep frontend concerns out of core.
- `crates/aicoder-providers` owns model-service clients and HTTP adapters.
  Providers depend on core; core must not depend on providers or HTTP clients.
- `crates/aicoder-runtime` owns layered application settings, credential references,
  default coding tools, filesystem session stores and Git completion verification.
  Keep terminal/Web types out; core must not depend on it.
  Concrete IO/process backends belong here; core builders install no implicit tools.
  Read-only tools must bound input scans and complete serialized pages, preserve usable
  continuation cursors, and report exhausted budgets and stale file cursors explicitly.
  Mutation preparation must be read-only; approval must describe the same owned plan
  later committed. Preserve conflict checks and cleanup on denial/cancellation.
  Shell capture must stay bounded while draining output; retain leader ownership until
  group signalling, and keep dropped-future cleanup independent of runtime shutdown.
  Session grants are explicit, ephemeral host authority: bind workspace/session/tool
  registry, keep caches bounded, and never restore authority from history or snapshots.
  Revocation must expire pending questions; cancellation/steering and cache eviction
  must not create late grants. Frontends submit choices, not authorization rules.
  Backend checkpoints must stay opaque, immutable and out of serialized domain history.
  Project settings may select trusted profiles but must not define credential routing
  or private storage paths. Keep credential values out of settings snapshots/errors.
  Resource snapshots must preserve user/tool history, keep skill assets inside pinned
  package roots, and load benchmark rules only from base checkouts with frozen globals.
- `crates/aicoder-cli` and `crates/aicoder-tui` are frontends over core/runtime;
  do not duplicate agent execution logic in either frontend.
- `crates/aicoder-eval` owns evaluation and SWE-bench integration. Do not
  leak evaluation fixtures or grading answers into agent prompts. Pin observed execution
  inputs and keep every selected failure in benchmark denominators. Bounded capture failures
  retain progress and explicit uncertainty; artifact failure never retries completed work.
  Replay is observation only. Unknown usage/prices and sampled process RSS are not exact cost
  or isolated agent peak memory. SWE-bench resume binds complete inputs and owned artifact
  receipts; missing/corrupt evidence never silently reexecutes a case. Keep generation failures
  in grading denominators, isolate official cache namespaces, and clean external containers
  only with authority created for the current fresh host invocation.
- Keep canonical recorded history separate from provider projections. Recovery repairs
  and pruning must not become historical facts; journal indices follow canonical input.
  Preserve declared runtime prefixes and branch provenance when constructing model views.
- Semantic summaries must use bounded, tool-free requests and charge every attempt to
  run usage/budgets. Commit branch-anchored checkpoints before using reduced context;
  keep raw history, interruption uncertainty and restored authorization separate.
- Output artifacts are explicit scoped host storage. Bound serialization, staging, disk
  admission and pages; keep raw bytes and capture completeness separate from previews.
  Archive failures must not repeat completed operations or publish partial data as complete.
- Navigation metadata must not become model instructions. Scope idle mutations to the
  workspace/run lease, retain old branches, and reject stale cursors. Validate model,
  adapter and projected-context compatibility before writing a selection or turn;
  history may select a model only inside an explicitly host-selected provider route.
- Token counts must identify scope, precision and fallback. Exact text encoding is not
  exact provider framing or billed usage. Share counting across preflight, compaction and
  budgets; custom policies cannot bypass known windows, protected prefixes or tool units.
  Keep optional vocabularies/dependencies outside default core/runtime builds.
- Archive loading must bound records, indexes, snapshots and pages. Window history contains
  only real materialized records; private metadata descriptors must never become prompts or
  canonical facts. Retain global provenance and raw archive access, and stream fingerprint
  verification without weakening conflicts, ownership or torn-batch recovery.
- CLI payload stdin must not become authorization input. Keep machine stdout limited to
  its declared format, report incomplete delivery, and cancel/await the execution owner
  before exiting. Reuse core sessions for REPL history instead of rebuilding a second loop.
- RPC request IDs must prevent duplicate execution with bounded state. Scope controls and
  approval choices to the owned run, keep protocol queues/serialization bounded, and await
  the owner after EOF, shutdown or transport failure. Notifications cannot fabricate delivery.
- Optional Web hosts fix principal/workspace/provider authority outside request payloads.
  Authenticate before reading bodies, bound retained runs and SSE bodies, and distinguish
  observer disconnect from owner cancellation. Await owners before HTTP/runtime shutdown.
- Exports preserve canonical records and all branches, never provider repairs or authority.
  Validate a pinned idle source before accepting immutable output; publish only by explicit
  adapter call. Keep text inert and report post-visibility failures without automatic retries.
- Model controls require declared capabilities and adapter validation before journal writes.
  Keep requested effort distinct from service default, visible reasoning and billed work.
  Bind branch choices to model epochs; request overrides must preserve live registry identity.
- TUI drafts, history and previews are bounded frontend state. Recognized paste events are
  payload, never approvals or automatic submissions. File pages are explicit immutable user
  input; keep continuation metadata and reject late captures after cancellation/navigation.
  Themes stay outside runtime/core settings. Unknown prices or incomplete usage cannot become
  billed cost; display counting scope/precision and distinguish previews from persisted data.
- Local images are explicit bounded immutable user snapshots. Keep codecs in optional runtime
  features; never fetch image URLs implicitly or count base64 as text tokens. Validate queued
  input capabilities before journal application, and keep cancelled captures out of drafts.
- Native extensions are explicit host-owned immutable registrations. Keep guards before
  preparation and approval-cache lookup; observations cannot mutate execution. Preserve
  declared prefix boundaries, cancel dropped hook children and rotate registry authority
  when installing a new snapshot. Never load native code or restore grants from history.
- Process tools use explicit host launch settings and conservative capabilities. Keep remote
  plan tokens bound to the original connection and out of domain records; never retry an
  uncertain operation implicitly. Bound both pipes/queues/plans, retain leader ownership
  through group signalling and report incomplete cleanup without restoring authority.
  Stock launch files are explicit host authority: preflight references before spawning,
  publish complete immutable catalogs, retain executable guards separately from registrations,
  and await owners after startup cancellation and normal/error exits. Reload must not restart
  programs implicitly or let cloned tools outlive a dropped executable owner.
- MCP metadata/annotations never grant authority. Freeze catalogs, revalidate freshness/epochs
  before dispatch, keep private result metadata out of history, and archive typed binary
  media without counting base64 as text. Completed-call capture failures cannot retry tools.
  Honor the selected protocol era and keep server interactions outside unoffered capabilities.
- Resource packages use explicit payload allowlists, exact content/dependency pins and private
  host storage. Installation never launches code. Resolve executable profiles only in the host;
  reload/restoration must revalidate trust and preserve old generations on failure. Public
  package state cannot restore tool grants, backend handles or credential-routing authority.
- Extension UI is ephemeral data, never authorization or canonical history. Bound queues,
  payloads, statuses and operation counts; expire questions on cancellation, timeout, owner
  replacement and dropped futures. Keep approval choices separate, preserve paste as data,
  and serialize command cancellation/submission before launching a normal AgentInput turn.
- Prepared-effect declarations come from the owned host plan, never model annotations or
  preview parsing. Guards retain the approved plan and fail closed for opaque mutation effects.
  Worker examples use fixed host profiles and separate sessions/controls/budgets; correlate
  provenance without importing parent authority, and report unknown usage/trace gaps honestly.
- Optional tool programs emit bounded intents into the existing dispatcher; they cannot bypass
  guards, owned preparation, leaf approvals, cancellation, archives or parent budgets. Admit
  ordinary and composed calls atomically. Keep JS dependencies outside default builds, record
  leaf facts separately from provider projection, and never retry completed effects on failure.
  Branch stores contain public JSON only; cancellation/steering must not commit late state.
- Keep public APIs and persisted session formats compatible when practical.
  Document intentional breaking changes and provide a migration path.

## Engineering

- Read the relevant code and tests before changing behavior. Keep changes
  scoped and use existing abstractions where they fit.
- Add focused tests for behavior changes, especially tool dispatch, session
  persistence, streaming, cancellation, and error paths. Avoid real model
  credentials and network access in automated tests.
- Treat model output and tool input as untrusted. Preserve approval, workspace
  path checks, output limits, and credential redaction. Bash runs with the
  user's permissions and is not a sandbox.
- Do not log credentials or commit `.env`, session data, or generated artifacts.
- Keep documentation and examples aligned with actual behavior. `TODO.md`
  records plans, not guarantees about implemented features.

## Verification

From the workspace root, run the checks relevant to the change. Before a
cross-crate change is complete, run:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
```

Git, Bash, and ripgrep are runtime prerequisites for tool and evaluation
workflows. The official SWE-bench harness additionally needs Python and Docker.
Report any checks that could not be run.

`docs/roadmap.md` is the development plan; `docs/development-baseline.md`
records the S0 API, storage and performance baseline. Use the pinned toolchain
in `rust-toolchain.toml` and inherit workspace metadata and lints in new crates.
