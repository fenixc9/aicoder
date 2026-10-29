# S3 project resources acceptance

This completes S3's AGENTS/SYSTEM/skills/templates item, including roots, nested scope,
deduplication, limits, source inspection, disable switches and explicit reload boundaries.
Concrete tool/JSONL extraction and remaining tool/approval improvements are still open;
S3 and the full Pi-parity roadmap are not complete. The comparison target remains
[Pi v1.4.0](pi-baseline.md); no TypeScript plugin/wire compatibility is promised.

## Delivered contract and evidence

- Global/ancestor/workspace instructions, nested AGENTS rules, nearest SYSTEM selection
  and APPEND_SYSTEM composition are frozen and labeled before model/tool work. Parent
  trees are not recursively scanned. Fixed exclusions, hard budgets and canonical
  identity avoid unbounded startup or partial context. JSON config inspection remains lazy.
- The global root owns its resource locations. Disabling global context cannot reload
  the same files through an ancestor alias; a symlinked explicitly trusted user root
  is canonicalized once, avoiding duplicate/unsafe rediscovery.
- Skills expose metadata initially, activate only on demand, verify the main fingerprint,
  and read bounded package assets without link/parent escapes. YAML parsing is real,
  event-based and bounded; aliases are rejected instead of expanded. No script executes
  and no frontmatter grants approval. A literal manual-only preference is honored for
  catalog/body invocation; ordinary package references remain bounded text resources.
- Templates support quoted arguments, positions/all/slices, literal dollars and bounded
  expansion. CLI and TUI invoke them explicitly; historical/dataset user content is not
  reinterpreted. Rejected queued template input retains the original draft invocation.
- Core accepts system prefixes only and moves existing user/tool/recovery history behind
  them. Source metadata is journaled before model calls and appears in progress/terminal
  metadata. Failure/cancellation stops preparation without inventing completed work.
- Runtime reload returns a new generation. TUI exposes /resources and idle-only /reload;
  existing agents keep their old snapshot and failed reloads do not replace the agent.
- Bench factories use each base checkout with parent context disabled and globals frozen
  for the batch. Actual provenance is recorded; private checkpoint matching now includes
  base_commit and input_context_version, preventing incompatible-context resume.

See [the complete discovery/safety/API rules](project-resources.md) and [migration](migrations.md).
Fingerprints detect ordinary changes; they are not cryptographic authentication or an
archive of every asset. Roots/limits do not sandbox Bash or preempt arbitrary filesystem
latency. Non-Unix fallback behavior is not established by Linux verification.

## Verification

All passed on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-runtime --example offline_resources
cargo run --locked -p aicoder-runtime --example offline_runtime
cargo run --locked -p aicoder-core --example offline_smoke
cargo run --locked -p aicoder-core --example controlled_agent
cargo run --locked -p aicoder-core --example event_stream
cargo run --locked -p aicoder-core --example event_stream -- --overflow
python3 scripts/check_event_schema.py
```

**293 workspace tests** pass. The 18 resource cases cover scope/precedence, switches,
metadata-only disclosure, real YAML/malformed aliases, collisions, arguments/expansion
bounds, retained text/reload, tree/file limits, UTF-8 pages, link/traversal rejection,
actual model input/session provenance, preserved host messages and isolated case context.
Three core context cases cover history integrity, cancellation/deadlines and failed
provenance journaling. CLI inspection and a local HTTP fixture verify real request bodies.
TUI tests cover reload refusal and raw-draft retention. Existing evaluation/session/tool/
provider regressions remain green. The event-v1 optional metadata is additive, and the
published frame fixture still validates. Hosted CI has not been claimed as already run.

The new offline example executes the real two-round load_skill loop, stores provenance
in a memory session and verifies no script ran. It requires no model credentials or
model-service network. Tests use local fixtures/temp files; real-model coding success
and official SWE-bench grading are not established by these checks.

## Lightweight measurements

The dependency graph grows from 235 to 239 packages: yaml-rust2 plus arraydeque,
hashlink and foldhash. A real YAML parser avoids a partial ad hoc frontmatter dialect;
its optional encoding feature is disabled, with bounded UTF-8 event parsing and no
YAML object/alias expansion. Shlex was already present and is reused. No script runtime,
HTTP service or database is added. Resource scans and retained text have separate bounds;
these do not claim a bound on the entire conversation/application archive.

The [three raw reports](baselines/s3-resources-report.json) retain the parent revision,
dirty-worktree flag and shared measured source digest. Compared with the shared-runtime
increment:

| Release executable | Size | Change |
| --- | ---: | ---: |
| CLI | 11.00 MiB | +4.57% |
| TUI | 11.42 MiB | +4.58% |
| SWE-bench | 11.28 MiB | +4.14% |

Samples of 125 offline smoke cases took 855.4, 331.1 and 338.0 ms (median 338.0 ms),
versus the preceding 328.0 ms median. The 855.4 ms sample is retained as observed;
three samples do not establish its cause or a statistically significant change.
Peak RSS was 5376, 5248 and 5248 KiB (median 5.125 MiB), equal to the preceding median.
Changes remain within the current review budgets. Existing-target build/help/idle
samples retain the collector's limitations; no clean-build or rendered-TUI comparison
is claimed. Scripted smoke is not a resource-loading throughput or real-model quality
benchmark. Separate timing samples of the three-file/two-round resource fixture are
retained in the raw report; they include process/temp-file overhead and do not establish
worst-case discovery or filesystem latency.
