# S7 installed executable activation acceptance

[Explicit host activation](host-extensions.md) now includes installed package process/MCP
entries in CLI JSON/RPC, TUI and Web. --activate-packages opts in after exact-content install
consent; optional --package-programs supplies host-owned logical dependencies. Package
selection/history does not install, authorize or start code. Default builds/starts remain
free of implicit executable activation, with no dependency/feature default change.

Runtime prepares package trust, host profiles and credential references before starting
direct stdio owners. It enforces one 32-process admission limit, combines immutable catalogs
and awaits all new owners on failure. Package guard drop invalidates retained tool adapters
and signals owned groups independently of Tokio; it does not cancel the host parent context.
Resource reload preserves the original executable generation, whose catalog still identifies
the old package digest and observed peer stamp. Adopting changed package code requires an
explicit new host generation. Host program versions are attestations, not automatic binary
identity verification. finish preserves run errors together with cleanup uncertainty.

Focused Rust tests verify:

- Package/MCP owner drop with retained tools, actual process reaping, failed later dispatch
  and unchanged host cancellation context.
- Package profile/trust preparation before any direct process; a later failed package
  handshake reaps both kinds without publishing a partial snapshot.
- Shared direct/package process limits before launch; current trust validation rejects a
  frozen package selection after uninstall/revocation.
- Text resource reload can select a new pin while retaining the original live executable,
  original content digest and peer metadata, with no new process started.
- Host profile file bounds/types, private Debug output, generic credential lookup errors,
  compiled-out transport rejection and invalid OS path admission.
- A failed cleanup cannot hide the original run error.

Pinned Rust 1.92.0 / Linux final checks passed:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo run --locked -p aicoder-runtime --example offline_packages
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-runtime -p aicoder-eval --features server,host-extensions,mcp-fixtures --bins --examples
python3 scripts/check_host_extensions.py
python3 scripts/check_host_extensions.py --packages
python3 scripts/check_frontend_parity.py
python3 scripts/check_tui_interaction.py
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
```

All 17 gates passed, including **638 default workspace, 712 all-feature and 226 isolated
core tests**. The package host driver installs a trusted fixture without executing it, then
actually calls its MCP write through each frontend. RPC/Web/TUI require their normal user
approval before the file exists. All child owners are reaped after normal exit and slow
startup cancellation; metadata starts none. The direct driver and existing TUI/SDK/CLI/RPC/
Web, coding-archive and SWE fixtures remain green. No real model/network credential was used.
CI adds the package variant; these local observations do not claim hosted CI execution.

The first workspace run exposed an existing output-lock inheritance race. A kernel descriptor
reproduction failed before the fix; explicit parent-owned unlock and an actual fork/drop
regression now pass. All 13 SWE contract tests passed in 10 consecutive stress runs (130
checks), followed by the successful final workspace gates. This correction is the separate
939254c commit; no automatic agent rerun was added and historical evidence was not altered.

The [footprint record](baselines/s7-package-host-report.json) retains all three final-source
default samples. Their shared source SHA-256 is
`ce5e2fcc2f3f8aba5cfc7e4da1df1cb3bfec9e96fdbd49cc979ac5a89369a2de`;
the revision is the parent because measurement precedes commit. Resolved packages remain
272 with no dependency or default feature change.

| Default metric | Observation | Change from comparison isolation |
| --- | --- | --- |
| Release CLI | 17.434 MiB | +0.137% |
| Release TUI | 17.524 MiB | +0.246% |
| Release SWE-bench | 17.252 MiB | -0.025% |
| 125-case smoke wall time, median | 394.39 ms | +1.416% |
| Smoke process peak RSS, median | 7,808 KiB | -3.175% |
| Idle offline runtime RSS, median | 4,224 KiB | +0.571% |

All smoke wall samples are retained: 414.26 / 394.39 / 389.62 ms. These cached host
observations are not latency guarantees or active frontend/server memory measurements.
The intervening output-lease fix is included in this comparison. Package theme selection,
remote acquisition, typed model-visible tool media and remaining Code Mode/hooks stay open
in the [Pi matrix](pi-parity-audit.md); no full parity or final release is claimed.
