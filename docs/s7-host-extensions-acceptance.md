# S7 stock stdio host acceptance

The [host launch contract](host-extensions.md) supplies actual CLI, TUI and Web paths for
runtime process/MCP adapters. Shared runtime builders install one immutable snapshot;
frontends own startup/shutdown rather than implementing another agent loop. All transports
remain optional. Core gained only immutable native-set composition and validation.

Focused runtime tests cover empty/default behavior, manifest validation, private Debug output,
compiled-out transport rejection, native collision checks, process+MCP catalogs in an actual
Agent, denied/allowed MCP writes, reload retaining the live snapshot, missing credential
references before any launch, partial-startup cleanup and dropped-owner invalidation/reaping.

The offline `scripts/check_host_extensions.py` uses a real MCP subprocess, scripted loopback
provider, CLI JSON, RPC, Web HTTP and a real TUI PTY. Eight model requests produce four
successful one-tool turns. RPC/Web/TUI require their normal explicit approval; the file is
absent before approval. All children are reaped after normal exit. A separate slow native
server verifies startup SIGINT in CLI/TUI and SIGTERM in Web; metadata commands launch no
child. No remote service, real model credential or browser is needed for these checks.

Final pinned Rust 1.92.0 / Linux checks passed:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-eval -p aicoder-runtime --features server,host-extensions,mcp-fixtures --bins --examples
python3 scripts/check_host_extensions.py
python3 scripts/check_tui_interaction.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
```

**630 default workspace, 697 all-feature and 226 isolated core tests** pass. The 15 final
checks passed on frozen source. CI adds the offline stock-host driver; local results do not
claim hosted CI ran. Existing TUI controls, SDK/CLI/RPC/Web normalization, benchmark archives
and SWE bridge fixtures remain successful. No real service credentials were used.

The [default footprint record](baselines/s7-host-extensions-report.json) retains all three
final-source samples. Resolved packages remain 272 with no dependency/lockfile change.
The shared Rust/manifests/embedded-assets fingerprint is
`94af9cb9134567afdb9bb08ae5b57547bf63ad1cfa5e1bec08e44c1d44d76475`;
the recorded revision is the parent because measurement precedes commit.

| Default metric | Observation | Change from SWE bridge |
| --- | --- | --- |
| Release CLI | 17.410 MiB | +0.379% |
| Release TUI | 17.481 MiB | +1.017% |
| Release SWE-bench | 17.253 MiB | +0.071% |
| 125-case smoke wall time, median | 390.69 ms | +0.756% |
| Smoke process peak RSS, median | 8,064 KiB | +3.279% |
| Idle offline runtime RSS, median | 4,268 KiB | +0.471% |

Smoke wall samples are **358.08 / 1150.54 / 390.69 ms**; the outlier is retained, not discarded
or attributed without evidence. Median changes stay below roadmap review thresholds, but these
unisolated cached Linux measurements do not establish a latency guarantee. Peak RSS comes from
GNU time for the offline smoke process; idle RSS excludes TUI/browser and configured servers.

Package activation, remote package acquisition, typed model-visible tool images and remaining Code
Mode/interception differences stay open in the [Pi matrix](pi-parity-audit.md). This feature
does not establish real-model coding quality or complete Pi parity.
