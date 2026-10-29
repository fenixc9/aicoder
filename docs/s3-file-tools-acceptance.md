# S3 bounded discovery, reads and search acceptance

Runtime now supplies directory listing and glob search, and bounds file reads and grep
before materializing their results. All successful pages retain a usable continuation
token inside the host's serialized-output budget. This completes S3's directory/glob/
bounded read/search item. Editing conflicts/diffs, shell contracts and session grants
remain open; S3 and full Pi parity are not complete.

## Delivered contract

- `ListDirectoryTool` and `GlobTool` join the shared default registry after the original
  five tools. Applications, CLI, TUI and evaluation use the same registry factory;
  read-only discovery does not require mutation approval.
- Async enumeration has entry/path/depth caps, stable result ordering, no recursive
  link following, typed errors and bounded project ignore sources. Actual glob and
  `.gitignore` parsers provide classes, alternation, nested rules and negation.
- `ReadFileTool` reads byte windows instead of an unbounded `lines().next_line()`.
  UTF-8-safe byte/line cursors continue within wide lines; distant initial line scans
  are bounded and partial lines cannot silently advance to the next line.
- `GrepTool` replaces collect-all process output with bounded asynchronous reads and
  regex compilation, capped line/excerpt/result retention, a resumable input-scan budget
  and counted binary skips. Structured records retain positions and read cursors;
  the legacy `matches` string remains available for display.
- Opaque query-scoped cursors reject mismatched settings and stale continuation files.
  Unix file identity distinguishes atomic replacements even when length/time match.
  JSON escaping, metadata and token bytes count toward each successful output cap,
  preserving structure through core dispatch rather than returning clipped JSON.

[File-tool rules](file-tools.md) and [migration](migrations.md) specify the limits,
error/recovery paths, regex/ignore differences, live-tree paging semantics, binary/long
line behavior, text newline handling and platform/concurrent-path limitations. There
is no core API, event schema or JSONL version change. This work does not create a process
sandbox, implement images, or promise a fixed snapshot of a concurrently changing tree.

## Verification

Passed locally on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p aicoder-core --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-runtime --example offline_file_tools
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo run --locked -p aicoder-runtime --example offline_runtime
cargo run --locked -p aicoder-runtime --example offline_resources
cargo run --locked -p aicoder-core --example event_stream -- --overflow
python3 scripts/check_event_schema.py
```

**308 workspace tests** pass; all 294 previous test names remain. The **14 new file-tool
integration tests** exercise escaped/Unicode wide-line reconstruction, CRLF/empty/final
lines, changed/forged/mixed-query cursors, atomic replacement, sorted nested-ignore glob
pages, cross-file grep continuation, output trimming without lost results, empty scan-
budget pages followed by later matches, recovery read cursors, binary detection,
invalid patterns/ignore/depth bounds, symlink escape and nonblocking FIFO rejection.
A 2100-file binary fixture verifies discarded read-ahead counts toward the input budget.
A dispatcher regression recovers all 1000 matching lines under a 2 KiB output cap.

**109 isolated core tests** still pass with no runtime/IO feature dependency. The new
example dispatches actual directory/glob/grep/read calls with deny-all mutation approval,
verifies ignored files stay excluded and copies a grep read cursor into a file read.
The existing five-case coding smoke retains edit/search/failure/cancel/resume behavior.
The event fixture still validates 18 v1 frames. Hosted CI, real model coding quality
and official SWE-bench grading have not been run or claimed here.

## Lightweight measurements

The graph grows from 239 to **248 packages**: globset/ignore plus bstr, crossbeam-deque,
crossbeam-epoch, crossbeam-utils, same-file, walkdir and winapi-util. Regex was already
present; the parser dependencies require regex-automata 0.4.18 (previously 0.4.16).
Using maintained parsers avoids implementing a partial glob/gitignore dialect. Only
bounded gitignore parsing is invoked from ignore; its parallel walker/thread pool is
not used. No external service, database or script runtime is added. Core retains its
existing direct dependency boundary.

[Three raw reports](baselines/s3-file-tools-report.json) retain parent revision,
dirty-worktree flag and the shared measured source digest. Relative to the backend
extraction increment:

| Release executable | Size | Change |
| --- | ---: | ---: |
| CLI | 11.60 MiB | +5.39% |
| TUI | 12.02 MiB | +5.15% |
| SWE-bench | 11.86 MiB | +5.28% |

Each sample runs the same five scripted cases 25 times. Times are 290.7, 284.9 and
1042.1 ms (median 290.7 ms, previous 325.4 ms); peak RSS is 6400, 6144 and 6528 KiB
(median 6.25 MiB, previous 5.25 MiB, +19.05%). Binary sizes remain under the 10%
review threshold. Median memory remains just below the 20% review threshold; the
6528 KiB individual sample is above it. The full 1042.1 ms timing outlier is retained
without assigning a cause. Three samples do not establish statistically significant
speed or memory changes. Parser/matcher code and structured results add in-process
cost, and regex now runs inside the process. The search result gains cursor/record metadata (the one-match smoke
transcript is 923 serialized bytes versus 469 before), so model-context payload is
not identical and no token-cost improvement is inferred.

The separate read-only discovery/search/read example takes 4.18, 3.77 and 3.55 ms
including process/temp-file overhead, with peak RSS 5376, 5376 and 5504 KiB. Its three
raw outputs are retained. Neither example establishes worst-case filesystem latency,
full-project throughput, rendered-TUI memory, model success rate or official bench
parity. The collector's existing-target build/help/idle limitations still apply.
