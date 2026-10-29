# S3 shared runtime configuration acceptance

> S3 relocation: current coding smoke is `cargo run --locked -p aicoder-runtime --example offline_smoke`;
> concrete backends now live in runtime. Commands/measurements below record this historical increment.
> See [backend migration](runtime-backends.md).

This completes the shared configuration/default-assembly item of S3. S3 remains open
for AGENTS.md/SYSTEM.md/skills/templates, concrete tool/storage extraction, tool/output
improvements and richer approval policy. The [runtime contract](runtime-configuration.md)
and [API migration](migrations.md) describe current behavior and intentional changes.

## Delivered behavior

- New workspace crate aicoder-runtime, used by CLI, TUI and SWE-bench. Core retains the
  execution loop and receives no application/runtime dependency. Frontends no longer
  depend directly on providers or dotenv loading for default assembly.
- Explicit > environment > project > user > default field resolution, distinguishing
  missing/null, with file/env/explicit source metadata. Model/API-base aliases respect
  actual process values above dotenv fallbacks. Alternative files, disabled discovery,
  canonical deduplication, regular-file and 256 KiB limits have defined behavior.
- Trusted user/host routing profiles, project selectors, independent credential
  providers and redacted snapshots. Empty references cannot route an unrelated native
  key to a custom endpoint. Defaults do not require a bearer token for inspection.
- Explicit nonmutating --env-file replaces automatic dotenv/crate-directory searches.
  Process values can rotate through the selected reference per request; existing agent
  settings remain immutable until a new snapshot is built.
- Default tool/session/context assembly is shared. Runtime supplies the credential
  reference exclusions to Bash, including unselected configured profiles. This is not
  a sandbox or a policy covering arbitrary unconfigured secret aliases.
- SWE-bench freezes effective settings, records their non-secret snapshot and applies
  model options/execution policy to every isolated case. Task-specific instructions
  remain; grading patch/answers are not inserted by configuration loading.

## Verification

All passed on Linux x86_64 with pinned Rust 1.92.0:

```sh
cargo fmt --all --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo run --locked -p aicoder-core --example offline_smoke
cargo run --locked -p aicoder-runtime --example offline_runtime
```

**268 workspace tests** pass. Runtime tests cover layered precedence and null clearing,
legacy aliases, source records, native key-family selection, trusted/project boundaries,
redaction, opaque presence lookups, request-time credential refresh, malformed/oversized
settings, disabled discovery, relative storage paths/no inspection writes, canonical
path deduplication, reload snapshots, FIFO rejection, and actual output-reservation
constraints. An offline injected-model/memory-store case uses the shared factory.
Subprocess tests verify Bash excludes both selected and unselected profile references
without relying on process-global credential-reference setup.

CLI/TUI tests inspect the same effective configuration without credentials or a terminal,
verify no session/log writes, ignore implicit .env and preserve process-over-file alias
priority. Dotenv tests verify no process export. Existing provider/execution/session/
evaluation regression suites pass, including shared model-option adapter construction.
Local tests use scripted models/local fixtures; no real coding-success benchmark or
future hosted-CI result is claimed. CI gains the offline runtime example.

## Lightweight measurements

The graph grows from 234 to 235 packages solely because of this workspace crate;
no new third-party package is added. Existing dotenvy/libc/url dependencies are reused.
The [three raw reports](baselines/s3-runtime-report.json) retain the pre-commit parent,
dirty flag, exact shared source digest and measurement limitations.

| Release executable | Size | Change from S2 events |
| --- | ---: | ---: |
| CLI | 10.52 MiB | +4.23% |
| TUI | 10.92 MiB | +4.29% |
| SWE-bench | 10.83 MiB | +4.57% |

Each sample executes 125 offline smoke cases. Samples took 328.0, 326.8 and 335.1 ms
(median 328.0 ms), compared with the preceding 328.7 ms median. Peak RSS samples were
5248, 5248 and 5376 KiB (median 5.125 MiB), equal to the preceding median. Changes stay
within current review budgets; these sparse samples do not establish a speed difference
or real-model success rate. The smoke measures existing core/storage paths, not worst-
case configuration loading or rendered TUI performance. Existing-target builds and help/
idle samples do not establish clean-build or interactive-startup timing.
