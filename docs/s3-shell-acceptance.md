# S3 bounded shell lifecycle acceptance

The shared shell now streams into bounded prefix/tail capture, distinguishes display
truncation from a combined output scan limit, owns group signalling through leader
reaping, and applies credential/startup exclusions. Shared per-tool deadlines support
long builds across CLI/TUI/bench. This completes S3's shell item; session grants and
full Pi parity remain open.

## Delivered contract

- Stdout/stderr use concurrent fixed-size reads and bounded retained data, with observed
  and omitted byte counts, loss-tolerant encoding and UTF-8-safe retained boundaries.
  Normal large logs can finish; excessive producers stop at the independent scan cap.
  The complete successful output/failure-details object fits the host result cap.
- Runtime exposes serializable `ShellLimits`, `ConfiguredBash`, `ShellOutput` and
  non-exhaustive `ShellStopReason`, plus public `registry_with_shell` for host assembly.
  Existing result fields remain available. A model does not select host resource policy.
- Exit observation keeps the direct leader waitable until group termination is requested.
  Normal/nonzero exit, command/host timeout, future drop and runtime shutdown share the
  ownership rule. Reaping/draining is bounded on returned paths; dropped children get a
  dedicated wait-only OS thread after synchronous signalling, independent of Tokio shutdown.
- The child has closed stdin, no implicit startup scripts/functions/options, standard
  credential-reference exclusions and the ordinary development environment. Trusted runtime
  profiles contribute credential names even when not selected; no credential values are logged.
- `execution.tool_timeout_ms` / `AICODER_TOOL_TIMEOUT_MS` follows existing config precedence
  and source inspection, defaults to 30000 and permits 1..86400000. Normal and isolated-case
  builders apply it; case repositories cannot change the frozen invocation value. Shell
  timeout input now accepts 1..86400 seconds and omission follows the host deadline.

[The complete shell rules](shell-tools.md) define output/error fields, budgets, deadline
layering, pipe/background behavior, signal/reaping status, exclusive child ownership,
credentials and platform limitations. Process groups are not a sandbox or a quota on
arbitrary descendants/memory/disk. Core interruption can supersede Bash's own capture
result. No provider, event-schema or JSONL version change is required.

## Verification

Passed locally on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p aicoder-core --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-runtime --example offline_shell
cargo run --locked -p aicoder-runtime --example offline_shell -- --capture-bytes 1048576
cargo run --locked -p aicoder-runtime --example offline_shell -- --capture-bytes 33554432
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo run --locked -p aicoder-runtime --example offline_edits
cargo run --locked -p aicoder-runtime --example offline_file_tools
cargo run --locked -p aicoder-runtime --example offline_resources
cargo run --locked -p aicoder-runtime --example offline_runtime
cargo run --locked -p aicoder-core --example controlled_agent
cargo run --locked -p aicoder-core --example event_stream -- --overflow
python3 scripts/check_event_schema.py
```

**339 workspace tests** pass, including **112 isolated core tests**. All 323 prior test
names remain. Fourteen new shell tests cover long dual-stream head/tail capture without
premature termination, escaped/Unicode windows, binary/signal results, scan-limit stops,
command/host timeouts with partial output, success/nonzero background cleanup, closed pipes
with a still-running leader, concurrent exit-status ownership, invalid budgets/arguments,
long-timeout input, denial without spawn, real-child environment/startup injection,
RunHandle cancellation and reaping after Tokio shutdown. Fixtures verify leader disappearance
from `/proc` and suppression of delayed same-group writes. They use owned temp files and fake
environment values. The prior dropped-group/stdin/nonzero/profile-exclusion cases remain.

Two new configuration cases verify timeout precedence/source/range/null behavior and actual
tool-context injection in both factories, including a case attempting to override the frozen
value. The new three-round example runs real shell commands through core, retains a long-log
tail and handles a captured timeout before the scripted model completes. Existing mutation,
read/search, session, resource, provider and evaluation regressions remain green. All 82
published event frames still validate. CI now runs the shell example; hosted CI, real-model
success rates and official SWE-bench grading have not been run or claimed here.

## Lightweight measurements

The dependency graph stays at **249 packages**. Runtime enables Tokio's signal feature
using existing packages; core acquires no process/IO/provider dependency. There is no new
crate, service, database or script runtime. Normal confirmed completion creates no reaper
thread; abandoned/unconfirmed children get a wait-only thread that ends after reaping.

[Three raw reports](baselines/s3-shell-report.json) retain parent revision, dirty-worktree
flag and matching final source digest. Relative to prepared edits:

| Release executable | Size | Change |
| --- | ---: | ---: |
| CLI | 12.34 MiB | -0.18% |
| TUI | 12.74 MiB | -0.12% |
| SWE-bench | 12.62 MiB | +0.15% |

The 125-case smoke samples take 385.1, 380.4 and 382.0 ms (median 382.0 ms versus
376.3 ms, +1.51%). Peak RSS is 6656, 7040 and 6784 KiB (median 6.625 MiB, unchanged).
These changes are within existing binary/time/RSS review budgets. The five-case smoke
has no Bash workload; it checks application/other-feature regression, not shell throughput.
Existing-target build/help/idle measurements keep the collector's limitations, and three
samples do not establish statistically significant performance changes.

The separate three-round shell example takes 1006.3, 1007.4 and 1007.7 ms, including
an intentional one-second timeout; peak RSS is 5248, 5248 and 5376 KiB. It demonstrates
behavior and recovery, not real model latency.

The capture fixture streams fixed-size Bash builtin output without printing the large log:

| Observed stdout | Three process times | Three peak RSS samples | Serialized result |
| --- | --- | --- | ---: |
| 1 MiB | 7.41 / 6.09 / 4.71 ms | 4608 / 4736 / 4736 KiB | 2387 bytes |
| 32 MiB | 58.97 / 57.38 / 55.03 ms | 4736 / 4608 / 4736 KiB | 2390 bytes |

Both median peak-RSS samples are 4736 KiB. Counts, omitted bytes, configured limits and
confirmed reaping are retained with every raw sample. These observations agree with bounded
capture for this fixture; they do not prove a universal RSS bound, enforce child memory
quotas, or benchmark arbitrary compilers/commands. The caller result cap is 4096 bytes and
its capture budgets are explicit; default application policy remains separately recorded
in the source/configuration contract. Full-output artifacts and progressive tool-output
UI remain later work rather than being inferred from internal streaming reads.
