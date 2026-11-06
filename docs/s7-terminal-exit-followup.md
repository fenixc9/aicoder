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
22.14% wall median increase and its long tails. The historical supplementary run selected
34ffa3d and 9b4b468 and requested the same standalone runtime release command with a shared target.
Review later found that both executable hashes were identical and the candidate command completed
in 0.19 seconds without compilation messages. Candidate source binding was not independently
established. The [historical report](baselines/s7-matched-smoke-report.json) is now explicitly marked
unverified for revision-comparison claims; all raw timing records, hashes and claimed revisions
remain unchanged.

| Historical observed label | Reference | Candidate |
| --- | --- | --- |
| Process wall median | 390.69 ms | 401.44 ms |
| Recorded case total median | 363.73 ms | 370.92 ms |
| Edit case median | 3.904 ms | 3.976 ms |
| SessionResume case median | 9.585 ms | 9.607 ms |

The +2.75% ratio of labelled medians and +0.38% median paired change describe those observed
samples. They do not prove that both requested revisions were executed or resolve the earlier
22.14% source comparison. The terminal-exit regression evidence above is unaffected.

A later [independent comparison](baselines/s7-message-finalization-matched-report.json) uses the
assistant-finalization commit versus the subsequent general message-finalization source digest,
with separate target directories and fresh compiler-artifact manifest checks for both libraries
and examples. It measures different source revisions; see [acceptance](s7-message-finalization-acceptance.md).
Shared host/filesystem, warm cache and lack of affinity/allocator/section profiling still limit
causal performance conclusions. Final Spark/Pi and release-quality acceptance remain open.
