# TUI terminal exit and matched smoke follow-up

Ctrl-C after an observed terminal state now requests exit even while the execution owner's final
result is still pending. Previously active_context remained set, so this key only cancelled and
required a second interrupt. A deterministic regression failed before the fix for Completed,
Failed, Aborted and Stopped. Active execution still cancels first. The existing shutdown path
continues to drain the execution owner, reference capture and input jobs before returning.
This proves the terminal-state timing window; it does not retrospectively prove that it caused
every earlier PTY timeout.

The interaction driver also waited for a full `Queued Steer` byte sequence. Real differential
terminal output reused the existing `e` cell and emitted `Queud Steer #1`. The actual submission
had succeeded. The driver now waits for the delivery/ID suffix and still asserts exact effective
steering/follow-up messages in captured model requests. It does not increase timeout limits.

Pinned Rust 1.92.0 / Linux passed **49 TUI target tests**, workspace formatting, TUI all-target /
all-feature Clippy with denied warnings and TUI rustdoc with denied warnings. Real PTY checks
passed for interaction, extension UI, raw-input UI, custom commands and generated images across
CLI/RPC/Web/TUI. See [verification receipt](baselines/s7-terminal-exit-report.json). Commands:

```sh
cargo fmt --all --check
cargo test --locked -p aicoder-tui --all-targets
cargo clippy --locked -p aicoder-tui --all-targets --all-features -- -D warnings
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server,images,code-mode --bins
cargo build --locked -p aicoder-tui --example offline_extension_ui
python3 scripts/check_tui_interaction.py
python3 scripts/check_extension_ui.py
python3 scripts/check_input_hooks.py
python3 scripts/check_custom_messages.py
python3 scripts/check_file_images.py --generate-images
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-tui --all-features --no-deps
```

## Smoke comparison

The preceding [delivery footprint report](baselines/s7-custom-delivery-report.json) retains the
22.14% wall median increase and its long tails. A supplementary comparison freezes source
revisions 34ffa3d (before busy custom delivery) and 9b4b468 (after delivery), builds both with the
same standalone runtime release command, copies/hashes the executables, warms them and runs ten
interleaved AB/BA pairs. Each process runs 125 scripted cases, 2,500 cases total. The subsequent
TUI-only fix is outside these source snapshots. No source changes or credentials were introduced
into comparison worktrees; temporary checkouts were removed after measurement.

| Observation | Reference | Candidate |
| --- | --- | --- |
| Process wall median | 390.69 ms | 401.44 ms |
| Recorded case total median | 363.73 ms | 370.92 ms |
| Edit case median | 3.904 ms | 3.976 ms |
| SessionResume case median | 9.585 ms | 9.607 ms |

The ratio of process medians is **+2.75%**; the median of the ten paired percentage changes is
**+0.38%**. All 20 samples, source IDs, executable hashes, build command, compiler/platform and
load observations are in the [matched report](baselines/s7-matched-smoke-report.json). These are
different statistics, so neither replaces the earlier three-sample workspace baseline.

The 22% magnitude was not reproduced under this comparison. Shared host/filesystem, warm cache,
no CPU affinity and standalone feature selection prevent a causal attribution or exclusion of a
smaller regression. No cold-build, allocator, binary-section or isolated RSS profile was done.
This does not measure real-model quality, vision, billing or the latency of active custom hooks.
Controlled profiling and final Spark/Pi/release acceptance remain roadmap work.
