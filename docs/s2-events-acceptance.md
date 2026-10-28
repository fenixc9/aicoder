# S2 final acceptance: versioned events and bounded observation

S2 is complete against the current roadmap. S3 shared runtime/project context is next;
S3–S7 remain outstanding, and this is not full Pi parity acceptance. Product comparison
is pinned to [Pi v1.4.0](pi-baseline.md). Our event schema and Rust API are independent
of Pi's TypeScript wire/plugin interfaces.

## Requirement audit

| S2 requirement | Current evidence |
| --- | --- |
| Append-only session tree, branch leaf, model/compaction/extension metadata | [Session contract](sessions.md), shared memory/JSONL contract tests, CLI tree/checkout and TUI reload regressions |
| Versions, atomic batches, conflicts, crash recovery, migration and backups | v3 JSONL tests for torn transactions, checksums, same-length stale edits, exact v1/v2 backups and a process exiting without Drop |
| Persist completed interactions on interruption and avoid unpaired model input | Journal checkpoints before tools and after completed results, provider/cancellation/task-drop tests, ephemeral missing-result repair and fixed interruption marker |
| RunHandle cancel/steer/follow-up/scoped approval | [Control contract](run-control.md), 16 external control tests, mutation barriers, single-use approval IDs, shared budgets and retained inputs |
| Versioned events, identity, sequence, complete lifecycle and terminal record | [Protocol v1](events.md), JSON Schema, normalized SDK fixture, EventTraceValidator, session-preparation/cancel/stop/final-persistence regressions |
| Bounded buffers, explicit loss and separate disconnect/cancel behavior | Independent count/byte limits, contiguous-prefix errors, concurrent producer tests, callback panic/timeout handling, TUI result reconciliation and dropped-observer tests |

Compaction/extension entries are storage facilities; semantic summaries and extension
loading retain their S4/S6 placement. Visual session-tree navigation and complete
CLI/RPC/Web interfaces retain their S5 placement. Event observation has no replay or
reconnect-resume guarantee. Callback code already executing cannot be forcibly killed;
its real-time drain limit and explicit diagnostics are documented.

## Verification

All passed on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-core --example offline_smoke
cargo run --locked -p aicoder-core --example model_provider
cargo run --locked -p aicoder-core --example embedded_agent
cargo run --locked -p aicoder-core --example controlled_agent
cargo run --locked -p aicoder-core --example event_stream
cargo run --locked -p aicoder-core --example event_stream -- --overflow
python3 scripts/check_event_schema.py
```

**246 workspace tests** pass. The new event suite has 22 cases covering versioned
round trips/fixture equality, invalid traces, count and JSON-byte limits, oversized
frames, independent/late/disconnected observation, concurrency, terminal sealing,
owner destruction, partial-stream cancellation, pending IDs and secondary persistence
failure, callback panic/timeout, and real-time draining under a paused Tokio clock.
The existing approval-deadline regression exposed the virtual-clock boundary and
passes with the corrected wall-clock callback wait.

TUI tests cover explicit stream failure, outcome ordering, dropping UI observation
without cancellation, and rebuilding a complete answer without erasing the draft.
Evaluation tests parse the canonical trace and validate it; SWE-bench tests verify
schema-3/event-version metadata and regeneration of legacy checkpoints. Observational
failure becomes an evaluator error instead of an apparently complete trajectory.

The schema check passed both with the installed development library and in an isolated
venv using the committed requirements. CI now checks the published fixture and both
event example modes. Offline examples require no model credentials or model-service
network. Smoke/evaluation fixtures exercise isolated temporary files, Git and local HTTP
servers; event-stream examples use memory sessions. External network during verification
was limited to development package installation and read-only upstream/version checks.
Local checks do not prove a future
hosted CI run or other-platform filesystem behavior.

## Lightweight measurements

No Rust package or agent/runtime service was added; the resolved graph stays at 234
packages. Serde's rc feature and UUID serde support enable canonical envelope encoding.
Python jsonschema is a separate development/CI checker, not a binary dependency.
No-op observers allocate no callback queue/thread; TUI now reads its subscription
without the old forwarding thread/channel. Persistent history/application archives
remain separate from pending-event buffer bounds.

The [three raw reports](baselines/s2-events-report.json) retain the parent revision,
dirty-worktree flag and a shared source digest. Compared with the preceding control
increment:

| Release executable | Size | Change |
| --- | ---: | ---: |
| CLI | 10.10 MiB | +2.09% |
| TUI | 10.47 MiB | +2.06% |
| SWE-bench | 10.36 MiB | +2.47% |

Samples of 125 offline smoke cases took 338.6, 319.2 and 328.7 ms (median 328.7 ms),
versus the preceding 341.1 ms median. Peak RSS was 5248, 5376 and 5248 KiB (median
5.125 MiB), unchanged from the preceding median. These samples stay within the existing
review budgets; they do not demonstrate a statistically significant speed improvement,
real-model coding success, or interactive TUI performance. Existing-target build times
and help/idle samples retain their original limitations; no clean-build comparison is
claimed. Buffer stress tests establish queue invariants, not whole-process memory bounds.
