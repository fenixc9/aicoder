# S3 session grants and stage acceptance

Host-owned, ephemeral session grants now support deny, allow once and allow this tool
for the session. Core enforces scope, bounds, revocation and admission; CLI/TUI display
and submit decisions. This completes S3. The roadmap advances to S4; full Pi parity and
S4–S7 are not complete.

## Grant evidence

- A grant matches host cache namespace, canonical workspace/Unix directory identity,
  logical session, tool name, capability and registry identity. It covers all arguments
  for that tool. Another cache, workspace, session, registry or capability cannot reuse it.
- Legacy bool handlers/responses remain one-shot adapters. Typed handlers and RunHandle
  support all three decisions. Unsupported session responses preserve the open request.
  Cancellation/steering is rechecked before grant insertion; late replies cannot grant.
- Bounded caches expose snapshots and revocation. Revocation/eviction closes or advances
  scope epochs and immediately expires pending questions, cleaning RunHandle state.
  Snapshots, traces and history carry no restorable authority. Existing preparation,
  conflict detection, budgets and execution checks still run on cache hits.
- CLI s is explicit about tool/all-argument scope. TUI adds s, /permissions and /revoke;
  successful reload/deletion closes relevant handles, and failed reload retains them.
  New optional event-v1 fields distinguish callback, handle and cached-grant resolutions.

See [the complete authority/lifetime contract](approvals.md) and [migration](migrations.md).
Caches are explicit host capabilities, not a replacement for server authentication,
workspace routing or an OS sandbox. Already admitted operations require run cancellation
to stop; revocation does not roll back their external effects.

## Verification

Passed locally on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p aicoder-core --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-runtime --example offline_approvals
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo run --locked -p aicoder-runtime --example offline_shell
cargo run --locked -p aicoder-runtime --example offline_edits
cargo run --locked -p aicoder-runtime --example offline_file_tools
cargo run --locked -p aicoder-runtime --example offline_resources
cargo run --locked -p aicoder-runtime --example offline_runtime
cargo run --locked -p aicoder-core --example model_provider
cargo run --locked -p aicoder-core --example embedded_agent
cargo run --locked -p aicoder-core --example controlled_agent
cargo run --locked -p aicoder-core --example event_stream
cargo run --locked -p aicoder-core --example event_stream -- --overflow
python3 scripts/check_event_schema.py
```

**355 workspace tests** pass, including **125 isolated core tests**. All 339 prior test
names remain. Thirteen new core cases cover decision lifetimes across turns, scope/tool/
registry/capability isolation, preparation on cache hits, bounds/eviction/inspection,
foreign/revoked/cancelled responses, unsupported choices, fresh-cache history continuation,
pre-history binding failures, cancellation/steering inside ready handlers, directory
replacement, immediate pending revocation, and contradictory trace metadata. Existing
callback, cancellation, mutation, shell and event regressions stay green.

A real CLI process/local HTTP fixture verifies s grants a second write without another
prompt, y then n leaves only the first write, and n leaves no file. Its listener/read
waits are bounded. Two new TUI tests verify scope/action visibility, no unbound s option,
failed/successful reload behavior and deletion invalidation. The three-turn JSONL example
uses actual file tools and RunHandle: two prompts, one cached reuse, then revocation and
preserved file content. It checks that the stored history contains no scope authority.

The normalized [98-frame session trace](protocol/session-approvals-v1-trace.jsonl) validates
with core and schema alongside the existing 18/64-frame fixtures: **180 default schema
frames**. The generated 98-frame trace passes separately. Metadata remains optional for
legacy event-v1 data; the validator rejects inconsistent decision/result/scope claims.
Hosted CI, real-model coding success and official SWE-bench grading are not claimed here.

## S3 completion audit

| Roadmap requirement | Current evidence |
| --- | --- |
| Shared layered configuration and credential references | [Runtime contract](runtime-configuration.md), config/CLI/TUI tests; effective snapshots and frozen isolated-case settings |
| AGENTS/SYSTEM/skills/templates, provenance, bounds and reload | [Resource contract](project-resources.md), resource integration/HTTP-body/TUI tests and offline_resources |
| Runtime tools/storage; reusable core contracts | [Backend mapping](runtime-backends.md), standalone core build/tests, shared memory/JSONL contracts and application factories |
| Directory/glob, bounded read/grep and continuation | [File contract](file-tools.md), oversized/Unicode/scope/link/scan and dispatcher pagination cases |
| Exact/conflict-aware atomic editing and approval diff | [Mutation contract](file-mutations.md), external writer/create/path/mode/cancel cases, prepared-plan/core and TUI rendering tests |
| Shell output/deadline/cancel/group/environment contract | [Shell contract](shell-tools.md), real process/reaping/large-output/environment cases and scripted model recovery |
| Deny/once/session policy; thin frontends | Current [authorization contract](approvals.md), core/CLI/TUI tests and persisted-session example |

Each prior increment retains its source-tagged measurements and limitations. Current
cross-feature tests/examples are all green. S3 does not establish semantic compaction,
full branch navigation/model switching, progressive tool-output UI, dynamic extensions,
MCP, Web/RPC or comparable real-task Pi benchmark success: those remain on their original
roadmap stages. The comparison target remains pinned [Pi v1.4.0](pi-baseline.md).

## Increment and cumulative lightweight review

The graph remains **249 packages**. This increment adds no package/crate/service/database;
UUID, serde, std collections and Tokio notification primitives were already present.
Default bounds are 32 cached sessions and 64 grants each; no authorization files or
background permission service are introduced. Core retains its IO/provider/frontend
boundary and is independently tested.

[Three raw reports](baselines/s3-approvals-report.json) record parent revision,
dirty-worktree flag and the matching final source digest. Compared with the shell increment:

| Release executable | Size | Increment change | Cumulative change from S2 |
| --- | ---: | ---: | ---: |
| CLI | 12.44 MiB | +0.86% | +23.24% |
| TUI | 12.84 MiB | +0.82% | +22.67% |
| SWE-bench | 12.70 MiB | +0.63% | +22.62% |

The 125-case process samples take 380.7, 385.7 and 1111.4 ms (median 385.7 ms versus
382.0 ms, +0.98%). The full 1111.4 ms outlier is retained without assigning a cause.
Peak RSS is 6656, 6784 and 6784 KiB (median 6.625 MiB, unchanged from shell). Increment
size/time/RSS changes stay within their review budgets. The three-turn grant example
takes 33.97, 33.61 and 34.90 ms, with peak RSS 6144, 6272 and 6016 KiB. It includes
process/temp-file/JSONL sync overhead, not human or real-model latency.

Across **all S3**, the preceding [S2 event samples](baselines/s2-events-report.json) have
10.10 / 10.47 / 10.36 MiB binaries, 328.7 ms median smoke and 5.125 MiB median RSS.
Cumulative smoke is +17.34%; cumulative binaries (+22.6–23.2%) and RSS (+29.27%, +1.5 MiB)
trigger the original 10%/20% review thresholds. These thresholds are not silently reset.

The default application now includes shared settings/resource assembly, maintained YAML/
glob/ignore/diff parsers, bounded discovery/capture, durable checked mutation, process
supervision and typed permissions. The [edit timing review](s3-edits-acceptance.md) records
the explicit sync/snapshot cost. Removing default capabilities or weakening checks to
meet an earlier footprint would conflict with the S3 default coding contract. Selective
host registries and core-only embedding remain available; a separate optional service/
engine or persistent authorization backend was not added. Further release/profile and
quality/performance comparison remains S7 work, with costs kept visible here.

Existing-target build/help/idle limitations still apply. The base smoke does not measure
full resource discovery, rendered TUI, a filled authorization cache, arbitrary child
memory or real model quality. Separate feature fixtures give scoped evidence only;
three samples do not establish confidence intervals or a universal performance bound.
