# S6 coding extension examples acceptance

The executable [coding examples](coding-extension-examples.md) now run planning, prepared-path
protection and independently managed read-only worker Agents through the existing core loop.
The only new core API is a default borrowed PreparedToolEffects declaration. No default tool,
new crate/dependency, script engine or duplicate execution loop is installed.

## Verification

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example coding_extensions
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server --bins --examples
python3 scripts/check_extension_ui.py
python3 scripts/check_extension_ui_browser.py
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_images.py
python3 scripts/check_web_browser.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_process_protocol.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**580 default workspace tests, 623 all-feature tests and 226 isolated core tests** pass.
Fifteen tests exercise the actual copyable policy/worker sources; two additional archive
budget cases cover an existing verification issue exposed during this work:

- Lexical protected paths, normalized traversal and canonical symlink aliases fail before
  mutation. Owned write/edit effects expose the captured target. Allowed prepared previews
  stay identical to the underlying file operation, and successful writes use that same
  operation with its original conflict checks and cleanup. Opaque shell commands never run.
- Planning's epoch rejects stale preparation even after an enable/disable cycle. Enabling
  planning waits for an admitted mutation rather than publishing a contradictory state.
  Retargeting an argument after preparation fails the file backend's route conflict check;
  changing a pinned protection route fails with policy_stale. Protected bytes stay intact.
- The running example proves cached session grants do not bypass the before-tool planning
  hook: one ordinary approved write succeeds, later planning/protected/alias requests fail,
  and only one approval is issued. A child review then reads actual temporary source through
  the default read tool without modifying the parent's entries.
- Workers use new sessions and fixed profiles, with parent session/leaf/revision retained
  only as correlation. They do not receive parent prompts, controls or grants. Foreign
  workspace/profile inputs and arbitrary model/credential/storage fields fail. Write and
  recursive worker calls are unavailable in the child registry.
- Actual core outcomes enforce round/tool/token/output/duration limits. Unknown provider
  usage stays unknown and token estimates retain their precision marker. Complete traces
  validate; overload/owner failure cannot be labelled complete or billed as exact zero.
- Concurrency, eight retained jobs and 32 lifetime admissions are bounded. Cancelling and
  closing the manager await owned work; history cannot recreate a retained live job.
  Dropping an unpolled task publishes uncertainty synchronously. Multiple waiters observe
  released capacity, and forgetting a completed owner leaves canonical storage intact.

The new example is executed in CI. Existing real terminal/browser checks and SDK/CLI JSON/
RPC/Web parity remain green. Process/event/RPC/export fixtures still validate 12/337/44/6
frames/entries. Automated checks use scripted models, temporary repositories and loopback
services, with no real credentials or remote model requests.

## Archive IO verification finding

Four existing shell capture tests intermittently failed because archive finalization consumed
the fixed 250 ms IO budget; temporary diagnostics measured approximately 250–252 ms before
storage_timeout. Concurrent writers also hit the fixed 100 ms metadata-lock bound. Reported command results and configured capture limits were preserved; no test assertion
was removed or converted to a retry.
This identifies the timeout phase; it does not attribute kernel/disk scheduling precisely.

ConfiguredBash::with_archive_timeout and FileArtifactStore::with_lock_timeout now let trusted
hosts choose independent 1 ms–5 s bounds. Defaults remain 250/100 ms and model arguments cannot
change them. Positive filesystem tests choose explicit 2 s/1 s budgets and still require
immutable references, complete captures and exact binary bytes. A deterministic delayed-finalize
store proves default expiry, successful extended finalization, staging cleanup and one command
execution per authorized invocation. Process signalling/reaping and pipe-drain bounds remain
unchanged; temporary phase diagnostics were removed.

## Default footprint

[Three final-source samples](baselines/s6-coding-examples-report.json) share Rust/manifests/
assets SHA-256 `ad1a6c519f3837905eb109459995bdf331eaa6fb1b0408049b75ba3c4e33ac2f`.
The recorded revision is the parent because measurement precedes the feature commit.
Cargo.lock and the 271-package resolved inventory remain unchanged. Copyable helpers compile
only in examples/tests; ordinary builders install none.

| Metric | Primary observation | Change from native UI |
| --- | --- | --- |
| Release CLI | 17.041 MiB | -0.02% |
| Release TUI | 17.003 MiB | -0.03% |
| Release SWE-bench | 14.275 MiB | -0.06% |
| 125-case smoke wall time, median | 595.54 ms | +52.55% — reviewed below |
| Smoke peak RSS, median | 7,936 KiB | +1.64% |
| Idle offline runtime RSS, median | 4,140 KiB | -1.24% |

Primary wall samples are 595.54 / 390.15 / 934.79 ms. All are retained. The first has one
211.71 ms Edit case; the third has a 119.93 ms Edit and 427.51 ms SessionResume. Individual
case medians remain 3.66–3.91 ms for Edit and 9.57–9.75 ms for SessionResume. This exceeds
the primary 20% review budget and is not hidden by resetting the baseline.

[Four additional alternating pairs](baselines/s6-coding-examples-paired.json) separately build
the exact parent and compare its executable with this source's executable. Parent walls are
597.82 / 372.19 / 398.95 / 391.54 ms; current walls are 393.37 / 387.73 / 386.50 / 620.69 ms.
Medians are 395.24 / 390.55 ms (-1.19%). Both show long tails; these
observations do not support a sustained default-loop regression. They do not assign the
precise kernel/disk cause. Build directory/cache/layout effects and allocator costs are not
isolated. Binary/RSS differences are observations, not optimization claims. No real-provider
latency, coding success or active-worker memory is measured.

## Scope

This completes the S6 example item. Planning is workspace/host-instance policy; worker jobs
are explicit command-level orchestration with their own reports, not an LLM delegate tool
pretending to share a parent model run's budget. Protected effects are trusted host plan
metadata, not a filesystem sandbox or a model annotation granting authority. Native code and
nonparticipating same-user processes retain their OS permissions. Long-lived hosts separately
manage storage across manager generations; forgetting a job does not delete its archive.

Code Mode and S7 evaluation/release/Pi comparison remain open. This is no real-model coding
benchmark, allocator measurement or guarantee of arbitrary third-party tool effect declarations.
