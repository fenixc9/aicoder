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
  leak evaluation fixtures or grading answers into agent prompts.
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
