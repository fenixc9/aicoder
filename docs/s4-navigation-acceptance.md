# S4 navigation and validated model selection acceptance

Core now provides workspace-scoped branch creation/checkout, global rename/labels, bounded
text search/tree pages, explicit recovery and validated model selection. CLI/TUI share these
operations and follow branch models within a fixed host-selected provider route. This
completes S4's navigation/model-selection item. Exact tokenizer/custom policy and archive
paging/loading budgets remain open; S4 and full Pi parity are not complete.

See [the navigation/model contract](session-navigation.md) for scope, cursor, metadata,
preflight, provider routing, grant retirement and compatibility rules. JSONL remains v3,
events remain v1, and no new dependency or provider is added.

## Verification

Passed on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-runtime --example offline_navigation
cargo run --locked -p aicoder-runtime --example offline_smoke
python3 scripts/check_event_schema.py
```

All **16 offline example executions** pass: navigation, artifacts, summary, branch context,
approvals, edits, file tools, shell, resources, shared runtime and smoke, plus core provider,
embedded/controlled agent, event stream and overflow. The new example is in CI. It uses
the real runtime/JSONL backend and a scripted provider to verify **alpha → beta → alpha**
through branching and checkout, source-branch exclusion from the model view, global rename
replay, labels/search and bounded tree pages. Its complete **18-frame** beta-turn trace
validates and exports a [sanitized fixture](protocol/session-navigation-v1-trace.jsonl).
The schema checker validates **277 frames** across all published fixtures; no envelope
schema change was required.

**428 workspace tests**, including **177 isolated core tests**, pass. All prior 404 tests
remain, with 24 additions:

- Seven navigation cases cover atomic branch creation, global name/label replay,
  ambiguity/unknown references, idle leases/workspace rejection, stale tree/search
  cursors, Unicode search across scan-page boundaries, opaque/reasoning exclusion,
  continuation after metadata-only empty pages, idempotent recovery without fabricated
  recorded results, and atomic label-quota validation/removal.
- Nine model cases cover branch-model following/restoration; foreign provider routing
  refusal; rejecting incompatible images/opaque state before existing-session writes;
  single input-context preparation with reasoning retained canonically; duration expiry
  before a turn write; known-window, tool, output-cap and policy mismatch; valid vision
  selection; cancellation/run ownership; and checking the actual host stream mode.
- Four runtime cases exercise append-only JSONL replay/old-prefix preservation, stale
  conflicts, failed metadata batches leaving bytes unchanged, retained registered specs,
  conservative profile capabilities and adapter-specific replay/option rejection. A
  loopback HTTP fixture proves a changed model uses the same service path and credential
  provider, with zero credential reads for metadata and one for actual generation.
- One CLI integration case runs rename/label/branch/recovery/model selection and tree/search
  continuation without credentials or a model service. Three TUI cases cover command
  argument validation, model-header update/failure without a fake chat turn, and shared
  navigation/model state plus retirement of old authorization handles.

The label map is a derived index of at most 256 entries, maintained from committed records;
queries and transaction validation no longer reparse the full navigation log for every label
operation. Normal compatibility validation borrows requests, avoiding another full history
clone unless reasoning adaptation or an empty-branch probe requires it.

## Lightweight measurements

[All three samples](baselines/s4-navigation-report.json) use the unchanged collector,
three warm-help samples and 25 iterations of the same five offline smoke cases. Final
Rust/Cargo source digest is `492b78d97d954db9c263a3f744df54f4ad220839db7c8958999a9057bbe0f2db`.
Reports identify parent commit `7ad9b57` with a dirty worktree; this digest identifies the
actual feature build. The reference is [s4-artifacts-report.json](baselines/s4-artifacts-report.json)
on the same host/toolchain. Resolved packages remain **249**.

| Release executable | Size | Change from artifacts increment |
| --- | ---: | ---: |
| CLI | 13.73 MiB | +1.96% |
| TUI | 14.12 MiB | +2.08% |
| SWE-bench | 13.36 MiB | -0.05% |

The 125-case smoke samples take **384.6, 389.1 and 614.4 ms**. The third is an elapsed-time
outlier and is retained. Median is 389.1 versus 379.5 ms (+2.53%). Peak RSS samples are
7552, 7296 and 7168 KiB (median 7.125 MiB versus 7.00 MiB, +1.79%). Median idle RSS is
4128 versus 4108 KiB (+0.49%). Current review budgets are not exceeded. Added compiled
navigation, validation and frontend paths explain additional code; tiny binary/layout or
timing decreases are not claimed as improvements. No clean-compilation claim is made.

The separate navigation JSONL fixture takes **89.61, 29.40 and 29.31 ms**, including
process/temp-file/synchronization overhead. The first is a large outlier and is also kept.
Peak RSS samples are 7040, 7296 and 7296 KiB. Every run verifies alpha/beta/alpha routing,
name/label replay, inactive-branch search and an 18-frame trace. Three observations do not
establish statistical performance equivalence or archive-scale throughput. The old smoke
case set is unchanged; these measurements do not cover worst-case tree setup, whole-archive
load residence, large multi-block searches, rendered-TUI memory or real-model behavior.

## Scope and limits

Search is literal/case-sensitive within individual visible Text blocks, one hit per message;
it does not index tool JSON, reasoning or opaque provider data, concatenate phrases across
blocks or implement fuzzy/regex search. Tree/search pages are bounded views over an already
loaded snapshot. Tree setup still traverses ancestry; loading the archive is not paged.
The roadmap now states that remaining memory requirement explicitly.

Preflight is local validation of declared model/protocol metadata, not remote discovery or
an availability/quality guarantee. Unknown windows/prices remain unknown. A model-specific
spec or compatible host context policy must be supplied where required. Profile selection
never guesses image/reasoning capabilities from names or imports a route from history.
Pure adapter validation and the actual request path enforce known wire restrictions.

Asynchronous instruction preparation is cancellable and included in the run duration;
synchronous repository I/O remains subject to backend behavior. Selection does not execute
semantic compaction, and a later compaction/provider call can still fail. Manual summaries
keep their separately configured host policy. TUI's grant retirement is explicit host policy;
new metadata cannot reconstruct approval authority.

History, metadata and inactive branches are retained without rewriting raw messages. This
increment does not claim total-history memory bounds, exact tokenization, full S5 interfaces,
real-model coding quality or Pi task-success parity.
