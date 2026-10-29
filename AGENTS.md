# Project Instructions

## Architecture

- `crates/aicoder-core` owns the reusable agent loop, model protocol, tools,
  events, cancellation, and sessions. Keep frontend concerns out of core.
- `crates/aicoder-providers` owns model-service clients and HTTP adapters.
  Providers depend on core; core must not depend on providers or HTTP clients.
- `crates/aicoder-runtime` owns layered application settings, credential references
  and default assembly. Keep terminal/Web types out; core must not depend on it.
  Project settings may select trusted profiles but must not define credential routing
  or private storage paths. Keep credential values out of settings snapshots/errors.
  Resource snapshots must preserve user/tool history, keep skill assets inside pinned
  package roots, and load benchmark rules only from base checkouts with frozen globals.
- `crates/aicoder-cli` and `crates/aicoder-tui` are frontends over core/runtime;
  do not duplicate agent execution logic in either frontend.
- `crates/aicoder-eval` owns evaluation and SWE-bench integration. Do not
  leak evaluation fixtures or grading answers into agent prompts.
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
cargo test --locked --workspace
cargo run --locked -p aicoder-core --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
```

Git, Bash, and ripgrep are runtime prerequisites for tool and evaluation
workflows. The official SWE-bench harness additionally needs Python and Docker.
Report any checks that could not be run.

`docs/roadmap.md` is the development plan; `docs/development-baseline.md`
records the S0 API, storage and performance baseline. Use the pinned toolchain
in `rust-toolchain.toml` and inherit workspace metadata and lints in new crates.
