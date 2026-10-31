# S6 local resource package acceptance

Local bundles now have a versioned manifest, explicit payload allowlist, exact package and
program dependencies, declared permissions and content-addressed host storage. CLI commands
inspect/install/list/select/enable/disable/remove and emit exact locks. Runtime freezes package
resources and validates replacements before adoption; SDK hosts can save/restore declared
public session state and explicitly activate process/MCP entries. See the [contract](packages.md).
No language runtime, downloader, lifecycle scripts or automatic executable startup is added.

## Verification

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_packages
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-runtime --all-features --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server --bins --examples
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_images.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_process_protocol.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**544 default workspace tests, 584 all-feature tests and 212 isolated core tests** pass.
The feature adds 17 runtime package tests, two CLI subprocess tests and four optional
process/MCP tests beyond the preceding 525/561 workspace inventory:

- Equal source bytes/modes produce equal pins; unlisted source files are not imported.
  Links, traversal, environment-file paths and FIFOs fail without publication. File/object
  budgets, malformed skill metadata and cancellation preserve the authoritative registry
  and clean staging. Concurrent imports publish complete serialized generations.
- Installed revisions remain available through exact locks. Enabled dependency closure is
  dependency-first, rejects missing/conflicting revisions and protects transitive trust
  from uninstall. Disabled updates actually disable the root. Duplicate package resource
  names fail reload; workspace skill/template definitions override package resources.
- Existing generations survive update/uninstall and failed replacement. Skill asset pages
  retain UTF-8 continuation offsets and reject undeclared files, bad offsets, tampering and
  symlink replacement. Global discovery can be disabled independently of an explicit host
  selection; logical selection honors existing settings precedence and creates no empty
  storage as a discovery side effect.
- Public state restores exact pins on the selected branch in both memory and JSONL stores.
  Stale/owned-session writes, undeclared or oversized fields and revoked pins fail.
  Canonical user/tool projection stays unchanged. A cold archive query succeeds with a
  1 KiB full-snapshot ceiling and unrelated 8 KiB history, proving actual bounded record
  materialization rather than an empty descriptor or whole-transcript fallback. Small legacy
  reads remain compatible; oversized legacy stores still need explicit migration.
- Real Rust child processes verify host-profile preflight, conservative tool approval,
  approved file mutation, MCP registration and awaited shutdown. Execution consent does not
  bypass a denied tool call. A failed second handshake reaps both new children and leaves
  the previous activation usable. No remote model, Node, Docker or credentials are needed.
- Actual CLI subprocesses inspect without installing, use default/locked/disabled selections,
  switch an exact trusted revision and reject uninstalled or oversized locks with clean
  stdout. The offline lifecycle example verifies reload, public state and trust revocation.

Existing TUI paste/reference/cancellation checks and SDK/CLI JSON/RPC/Web parity remain green.
Process/event/RPC/export fixtures still validate 12/337/44/6 frames/entries. No existing
session, event or RPC format version changes; additive public API/provenance changes are in
[migrations](migrations.md). The CI runs the new offline lifecycle example.

## Default footprint

[Three final-source observations](baselines/s6-packages-report.json) share Rust/manifests/
assets SHA-256 `86a4a13ec7b402a67d0ccfb289f410b26b427cc7824d0db3a3c3df549863a53c`.
The recorded revision is the parent because measurement precedes the feature commit.
Cargo.lock adds only the runtime edge to already locked ring 0.17.14. Its SHA-256 use now
activates ring/getrandom 0.2/untrusted in default runtime builds; core has no crypto dependency.
The complete resolved version inventory, including optional/platform packages, stays at 271.

| Metric | Final measurement | Change from S6 MCP |
| --- | --- | --- |
| Release CLI | 17.215 MiB | +3.20% |
| Release TUI | 16.486 MiB | +2.13% |
| Release SWE-bench | 14.248 MiB | +2.59% |
| 125-case offline smoke wall time, median | 387.76 ms | -1.34% |
| Offline smoke peak RSS, median | 7,680 KiB | +1.69% |
| Idle offline runtime RSS, median | 4,128 KiB | +0.88% |

Wall observations are 387.29 / 391.71 / 387.76 ms; peak RSS is 7,680 / 7,680 / 7,936 KiB;
idle RSS is 4,124 / 4,168 / 4,128 KiB. Package management, resource integration and integrity
code account for added default functionality; these observations do not isolate individual
components' costs. Existing targets/page caches are reused. Timing/RSS noise does not establish
an improvement or sustained regression. No clean build, installed-package scan throughput,
active host/child memory, real-provider latency or benchmark success is measured here.

## Scope

This completes the local package lifecycle and explicit SDK restoration path in S6.
Remote Git/npm acquisition, frontend UI capabilities/automatic executable activation, remaining
extension examples and Code Mode remain subsequent work; S7 and full Pi parity are open.
Theme assets can be read by hosts, but installing a package does not automatically change
frontend rendering. Restored public data does not resume child-process internals, private
backend checkpoints, credential routes or session grants. The host explicitly starts/adopts
new code at an idle boundary; current executors keep their owned snapshots until replaced.

Storage is bounded and retains old/orphan objects; no automatic GC deletes live generations.
Integrity pages hash the whole bounded asset before slicing. Programs are trusted host code
with user permissions; existing process backend limitations still apply. Directory durability
failure after registry visibility is reported for inspection, without automatic replay.
