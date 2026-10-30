# S5 CLI execution acceptance

CLI now supports bounded stdin/positional/flag input, final-text print, event-v1 JSONL,
sequential REPL over core sessions, shared runtime configuration and owning Ctrl-C
cancellation. This completes S5's CLI item; RPC, Web, richer TUI, images and exports remain
open. See [CLI behavior and compatibility](cli-execution.md).

## Verification

Passed on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
python3 scripts/check_event_schema.py
```

**454 default workspace tests**, **456 with optional features**, and **187 isolated core
tests** pass. The prior 447/449 tests remain, with seven actual-binary loopback additions:

- Positional and stdin prompts reach the provider body while `--print` stdout contains
  exactly the current final text, without token/reasoning/tool summaries.
- JSON stdout parses entirely as SDK envelopes and passes complete EventTraceValidator
  validation without appended plaintext.
- Piped REPL turns preserve previous messages/answers through an ephemeral core session;
  exit and subsequent unread lines do not become prompts, and no session files are created.
- Empty/oversized/conflicting input is rejected before a model service is contacted.
- SIGINT after an actual model request cancels the owner, produces a complete aborted
  JSON trace, and exits 130 within the test deadline.
- A piped `y` cannot approve a fixture write: its next provider request contains the denial,
  the file remains absent, the line instead becomes the next ordinary user turn, and both
  run traces validate independently.
- A stalled JSON downstream cannot retain the execution owner/global stdout lock; the
  child exits nonzero with an incomplete-delivery diagnostic within the test deadline.

Existing real CLI configuration/resource/session/artifact/proxy and unanswered-approval
budget tests remain green. The normal runtime smoke passes. No real model credentials or
remote provider calls are used by these tests. Event-v1 and JSONL-v3 schemas are unchanged.

## Scope and limits

Unix input uses a separately opened nonblocking description; original stdin flags are not
modified. Pending input is bounded, approval lines are separately capped, and abandoned
question readers stop when their receiver closes. New noninteractive payload/machine modes
do not implicitly read authorization from that payload. Core retains all permission rules.

JSON output uses core's bounded observer delivery. Broken output or incomplete delivery
fails the process; an indefinitely slow downstream consumer is not promised a complete
trace, and the CLI does not synthesize a replacement terminal frame. Diagnostics/tracing
stay on stderr. REPL errors can be followed by another turn; normal EOF/exit ends its input
session successfully. Idle interruption exits input, while a running turn is cancelled and
REPL can continue. Synchronous host/backend work is not preemptible by a signal.

TTY approval/interactive line discipline is supported by the adapter but these loopback
fixtures exercise pipe/noninteractive behavior, not a full PTY rendering benchmark. The
JSON event mode is not the planned RPC request-ID/control protocol. No frontend model/tool
loop or session replay authority is introduced.

## Footprint

[Three final-source samples](baselines/s5-cli-report.json) use the same pinned toolchain
and default features as the [S4 archive reference](baselines/s4-archive-report.json).
The source SHA-256 is `bc17f5df01f450faa8f036b8456b41ddc102f36705c5dc7b419b7adcf300557c`.
The recorded revision is the parent commit because measurements precede this feature's
commit; the source digest identifies the measured code and manifests.

| Metric | Final measurement | Change from S4 archive |
| --- | --- | --- |
| Resolved default packages | 249 | unchanged |
| Release CLI binary | 14.341 MiB | +1.87% |
| Release TUI / SWE-bench binary | 14.452 / 13.524 MiB | unchanged |
| Offline smoke wall time, median | 393.32 ms | +0.97% |
| Offline smoke peak RSS, median | 7,552 KiB | +1.72% |
| Idle runtime RSS, median | 4,108 KiB | +0.59% |

Each smoke sample runs 25 iterations of five scripted cases. All three wall-time samples
are retained: 1,117.89 / 386.54 / 393.32 ms. Peak RSS samples are 7,552 / 7,680 / 7,296 KiB;
idle RSS samples are 4,108 / 4,108 / 4,020 KiB. These local measurements include process/time
wrapper overhead and show variability; they do not establish a latency regression or
improvement, real-provider performance, interactive TUI cost, or task success rate.
Builds reused the existing target directory and do not measure clean compilation.

The CLI adds a direct libc dependency edge and signal feature, both already present in
the default dependency graph. No new resolved package version, default core/runtime
dependency or tokenizer vocabulary is introduced.
