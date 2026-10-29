# S3 concrete backend extraction acceptance

Default coding tools, workspace path helpers, JSONL persistence/legacy codecs and
Git workspace-change verification now live in `aicoder-runtime`. Core retains the
single execution loop, public tool/store/policy contracts, session domain and memory
repository. This completes the S3 extraction item; remaining tool and approval work
is still open. Pi parity and S3 as a whole are not complete.

## Delivered behavior

- Core builders start with an empty registry. Runtime has one shared coding registry
  assembly path, with credential-aware Bash and resource tools for application hosts.
  CLI, TUI and SWE-bench use that assembly; mock/custom SDK hosts can use core alone.
- Concrete tools use public `ToolContext` limits and runtime `WorkspacePaths`. Their
  schemas, order, approvals and IO behavior are retained. Core exposes result/error
  constructors so a tool implementation needs no privileged crate access.
- JSONL uses public snapshot/transaction APIs. File offsets, fingerprints and separator
  state remain opaque backend attachments, absent from domain history. Cloned views
  keep their prior checkpoint; reconstructed snapshots have no backend or run owner.
- Persisted format stays v3. Existing transaction/checksum, conflict/run lease, torn-tail
  recovery, private-file and v1/v2 exact-backup contracts remain exercised after the move.
  Session files need no migration for the Rust module relocation.
- Concrete and shared-backend tests move with their implementation. Pure execution,
  dispatch/control/event/budget and memory API tests remain in core. CI separately
  tests core without workspace Tokio feature unification.

The [API mapping and assembly guide](runtime-backends.md) and [migration notes](migrations.md)
cover the intentional Rust import/builder changes. Bash remains a host process, not a
sandbox. Stronger bounded reads/search, editing conflict checks, shell cleanup and
session grants remain separately tracked; this extraction does not establish them.

## Verification

All checks passed locally on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo test --locked -p aicoder-core --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo run --locked -p aicoder-runtime --example offline_runtime
cargo run --locked -p aicoder-runtime --example offline_resources
cargo run --locked -p aicoder-core --example model_provider
cargo run --locked -p aicoder-core --example embedded_agent
cargo run --locked -p aicoder-core --example controlled_agent
cargo run --locked -p aicoder-core --example event_stream
cargo run --locked -p aicoder-core --example event_stream -- --overflow
python3 scripts/check_event_schema.py
```

**294 workspace tests**, including **109 isolated core tests**, pass. All 293 previous
test names remain present. The added public checkpoint test verifies replacement/clone
independence, type-safe lookup, opaque Debug and reconstruction without owner/checkpoint.
The public embedded executor test now exercises the omitted registry and checks that no
tool definitions reach its provider. Runtime contract tests retain both memory and JSONL
coverage, provider-failure checkpoints, dropped-run recovery, partial parallel results,
steering and queued follow-ups. SWE-bench's fixture still creates the expected patch
with explicitly installed coding tools; no model service or official grading was run.
The published fixture validates 18 event-v1 frames. Hosted CI has not been run here.

## Lightweight measurements

The workspace still resolves **239 packages**. Core normal direct dependencies decrease
from 13 to 11 by moving fs2/libc ownership. Its Tokio features are macros/rt/sync/time,
without full, fs, process, net or rt-multi-thread. `cargo tree --locked -p aicoder-core -e normal` contains no runtime/providers/fs2/HTTP client. Transitive platform dependencies
can still exist; core still canonicalizes workspace identity. No extra crate or package
was added solely for the extraction.

[Three raw reports](baselines/s3-backends-report.json) retain the measured parent revision,
dirty-worktree flag and matching final source digest. Compared with the resource increment:

| Release executable | Size | Change |
| --- | ---: | ---: |
| CLI | 11.01 MiB | +0.02% |
| TUI | 11.43 MiB | +0.08% |
| SWE-bench | 11.26 MiB | -0.14% |

Each sample runs 125 offline coding cases. Times are 322.7, 325.4 and 327.1 ms
(median 325.4 ms, previous median 338.0 ms). Peak RSS samples are 5632, 5376 and
5248 KiB (median 5.25 MiB, previous 5.125 MiB). These stay within the current
10% binary/20% runtime-memory review thresholds; three observations do not establish
a statistically significant improvement or regression.

The relocated coding-smoke example itself grows from 5.82 to 6.21 MiB (+6.83%) while
using runtime as its backend host. It is an example executable, separate from application
binary measurements and pure-core embedding. Reports retain existing-target build/help/
idle measurements and their limitations; no clean-build, rendered-TUI, worst-case IO,
resource-loading throughput or real-model quality claim is made.
