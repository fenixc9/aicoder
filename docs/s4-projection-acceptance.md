# S4 canonical history and branch projection acceptance

Core now separates recorded history from provider-facing context. A branch can be
inspected/executed without checkout, and session continuation uses safe recovery messages
without storing or returning them as recorded facts. This completes S4's first item;
semantic compaction, durable context checkpoints, artifacts/navigation and tokenizer policy
remain open. S4 and full Pi parity are not complete.

## Delivered evidence

- Borrowed `SessionBranch` walks a chosen ancestry without changing revision/leaf or
  cloning sibling branches. It exposes raw message iteration, selected model metadata,
  owned safe context and borrowed recorded/recovery origins. Empty/unknown branches are explicit.
- Projection preserves recorded tool arrival facts, orders provider results by call ID,
  synthesizes fixed missing-result/interruption context, and rejects malformed or falsely
  completed tool units. Active-turn context requires a matching open turn with paired calls.
  Existing Compaction/extension records keep their metadata-only behavior.
- `ExecutionInput` carries separate history/request buffers and a validated explicit
  runtime system-prefix boundary. Existing SDK methods remain; new outcome/control/journal
  entry points and `from_branch` support host projections without implicit persistence.
- Agent validates prior history before new turn writes. Journal/checkpoint positions use
  canonical input, even when recovery expands context or controls append fresh messages.
  Instruction reload affects both declared prefixes without losing model-only markers.
- Pruning/reasoning adaptation affect only provider messages. Budgets estimate that actual
  view. The executor removes the redundant initial-view clone and moves outgoing messages
  into the provider rather than cloning the whole request-message vector again.
- Optional progress/event-v1 terminal reports carry source/count/estimate metadata without
  another full provider view. The validator checks request/compaction counts, last-request
  fields and bound source identity. Old progress/terminal JSON remains readable; JSONL is v3.

See [the exact ownership, input, provenance and compatibility rules](model-context.md).
This work does not add a semantic summary generator, activate old summary records, page
history or claim total long-history memory is bounded.

## Verification

Passed locally on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p aicoder-core --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-runtime --example offline_context
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo run --locked -p aicoder-runtime --example offline_approvals
cargo run --locked -p aicoder-runtime --example offline_edits
cargo run --locked -p aicoder-runtime --example offline_file_tools
cargo run --locked -p aicoder-runtime --example offline_shell
cargo run --locked -p aicoder-runtime --example offline_resources
cargo run --locked -p aicoder-runtime --example offline_runtime
cargo run --locked -p aicoder-core --example embedded_agent
cargo run --locked -p aicoder-core --example controlled_agent
cargo run --locked -p aicoder-core --example event_stream -- --overflow
python3 scripts/check_event_schema.py
```

**364 workspace tests**, including **134 isolated core tests**, pass. All 355 prior test
names remain. Nine new integration cases cover alternate-branch execution/sibling exclusion,
source/origin/reordering facts, missing-result recovery absent from canonical progress/history,
steering checkpoint alignment, live/legacy turn boundaries, invalid history failing before
new writes, runtime-prefix/derived-marker separation, projected token budgets/pruning,
reasoning-only request filtering and malformed/legacy terminal metadata.

The real JSONL example compares bytes/revision/selected leaf before and after an unbound
counterfactual run, then resumes the selected partial branch through Agent. It verifies that
inactive sibling facts remain stored but never reach that model, and that temporary repairs
never enter the session file. It validates the complete SDK event lifecycle and can export
its 17-frame trace. Existing session, permissions, file, shell, resource, provider and evaluation
regressions remain green. The current normalized base fixture includes optional context data;
legacy approval fixtures omit it and remain readable. The additional
[17-frame projection fixture](protocol/model-context-v1-trace.jsonl) brings default schema
validation to **197 frames**; the generated 17-frame trace passed independently.
Hosted CI, real-model performance and official SWE-bench grading are not claimed here.

## Lightweight measurements

The graph remains **249 packages**; no crate/service/database/tokenizer dependency was
added. Branch IDs/origins borrow from an existing snapshot, and terminal reports keep only
counts/source identifiers. Canonical, working and wire-owned messages still consume memory;
there is no zero-copy or paged-history claim. Clone removal is an implementation change,
not an allocator-instrumented measurement of message-copy volume.

[Three raw reports](baselines/s4-projection-report.json) record parent revision,
dirty-worktree flag and matching final source digest. Relative to S3 completion:

| Release executable | Size | Change |
| --- | ---: | ---: |
| CLI | 12.47 MiB | +0.24% |
| TUI | 12.86 MiB | +0.10% |
| SWE-bench | 12.74 MiB | +0.36% |

The 125-case smoke samples take 402.7, 379.2 and 379.1 ms (median 379.2 ms versus
385.7 ms, -1.69%). Peak RSS is 6912, 6784 and 6784 KiB (median 6.625 MiB, unchanged).
Changes stay within the current review budgets. Three observations do not establish a
statistically significant performance improvement; S3's cumulative cost review still applies.

The separate JSONL branch/recovery fixture takes 17.32, 16.71 and 17.60 ms including
process/temp-file/synchronization overhead, with peak RSS 5120, 5248 and 5120 KiB.
Its actual context report is retained with every sample (five initial history messages,
seven model messages, one recovered result and one interrupted prior turn). Existing-target
build/help/idle limitations remain. These short scripted cases do not establish worst-case
long-history construction/pruning throughput, total archive residence, rendered-TUI memory,
semantic-summary quality or Pi coding success rate.
