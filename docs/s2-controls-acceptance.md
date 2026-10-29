# S2 RunHandle acceptance

> S3 relocation: current coding smoke is `cargo run --locked -p aicoder-runtime --example offline_smoke`;
> concrete backends now live in runtime. Commands/measurements below record this historical increment.
> See [backend migration](runtime-backends.md).

Historical increment record. [Final S2 acceptance](s2-events-acceptance.md) supersedes
the remaining-stage status below; these measurements and test counts remain archived.

RunHandle cancellation, bounded steer/follow-up and asynchronous approval are now
implemented in the shared core and used by TUI. S2 remains open for versioned event
schemas, bounded event observation and disconnect behavior. This increment does not
claim full Pi parity or completion of later frontend/runtime/extension stages.

## Evidence

All checks passed on the pinned Rust 1.92.0 toolchain, Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo run --locked -p aicoder-core --example offline_smoke
cargo run --locked -p aicoder-core --example controlled_agent
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
```

The workspace has **219 passing tests**. The 16 external control integration tests
cover priority/FIFO, queue count/byte boundaries, preserved rejected drafts, steering
while a model/approval is pending, finishing an active mutation and skipping unstarted
tools, scoped one-use approval IDs despite repeated model call IDs, inherited
cancellation, task/handle destruction, retained inputs after session preparation or
journal failure, terminal callback closure, and shared budgets across follow-ups.
Memory and JSONL both verify persisted input provenance and correct multi-answer
history without duplicate checkpoints. TUI has an additional queue/rejection/application
regression and uses the core approval protocol instead of its own oneshot bridge.

The new offline example executes one explicitly approved in-memory operation and a
follow-up over three model rounds. It makes no model-service requests or file edits.
See [the full API and limits](run-control.md) and [migration notes](migrations.md).

## Lightweight measurements

No runtime or test dependency was added; Cargo still resolves 234 packages. Uncontrolled
runs allocate no input queue. Queue state is bounded by count and encoded bytes; each
boundary also limits how many controls it can drain. Ordinary successful controlled
session runs batch the final candidate into their existing terminal transaction.

The [three raw reports](baselines/s2-controls-report.json) retain the parent revision,
dirty-worktree flag and the same measured source digest. Against the preceding
[session increment](baselines/s2-sessions-report.json):

| Release executable | Size | Change |
| --- | ---: | ---: |
| CLI | 9.89 MiB | +1.38% |
| TUI | 10.26 MiB | +1.71% |
| SWE-bench | 10.11 MiB | +1.35% |

Consecutive samples of 125 offline smoke cases took 323.8, 677.8 and 341.1 ms. Their
341.1 ms median is 6.8% above the previous 319.2 ms median. The 677.8 ms sample is
retained as observed; these three samples do not establish a statistically significant
change or its cause. Peak RSS samples were 5120, 5248 and 5248 KiB; their
5.125 MiB median is 2.5% above the previous 5.00 MiB median. Changes remain within the existing review budgets.

These smoke workloads measure the default execution/storage paths, not control-queue
throughput or rendered TUI performance. Help/idle samples and existing-target build
timings are retained with their limits; no clean-build speed comparison is claimed.
The old event transport and TUI event channel still have unbounded buffering; input
queue bounds do not imply bounded event transport. Addressing that is the next S2 task.
