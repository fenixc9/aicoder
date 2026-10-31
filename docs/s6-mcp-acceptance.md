# S6 MCP stdio acceptance

The optional runtime MCP adapter now exposes real MCP tools through the existing core
agent loop and native registry. It supports modern 2026-07-28 metadata/discovery and four
legacy initialization versions, using the shared bounded process owner. The [contract](mcp.md)
describes profiles, content, freshness, authority and limits. No HTTP transport, implicit
server startup or session/event schema version change is introduced.

## Verification

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo test --locked -p aicoder-runtime --features mcp-fixtures --test mcp
cargo test --locked -p aicoder-runtime --features mcp-fixtures --test process_extensions
cargo test --locked -p aicoder-runtime --features process-extension-fixtures --test process_extensions
cargo run --locked -p aicoder-runtime --features process-extensions --example offline_process_extensions
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo build --locked -p aicoder-runtime --features mcp-fixtures --bin aicoder-mcp-fixture
cargo run --locked -p aicoder-runtime --features mcp-stdio --example offline_mcp
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-runtime --features mcp-stdio --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server --bins --examples
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_images.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_process_protocol.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**525 default workspace tests**, **561 all-feature tests** and **212 isolated core tests**
pass. Thirteen optional real-MCP-process tests extend the prior 525/548/212 inventory:

- Modern server/discover and per-request metadata work; legacy errors of different codes,
  silent probes, late probe replies, old selected versions and server pings complete the
  correct initialization/notification sequence. Host-forced era/identity mismatches fail.
- Pagination is bounded, repeated cursors fail, long remote names receive stable bounded
  aliases and original names remain intact on the wire. Host overrides are explicit; a
  server's readOnlyHint never bypasses core approval.
- Approved writes modify the real temporary file; denied writes send no tools/call. JSONL
  history retains canonical tool calls/results without initialization instructions, private
  tool metadata or private result _meta. The same core tool budget blocks a call before
  dispatch and EventTraceValidator accepts the complete authoritative event sequence.
- Text, structured data, resource links/text and binary image/audio/resource data are
  retained with their actual host artifacts. No resource URI is fetched. Binary base64 is
  excluded from model text. Missing storage/media budgets explicitly mark incomplete
  delivery of a completed call, and do not repeat it; isError stays a tool failure.
- TTL zero and list-change events revalidate metadata. Changed catalogs invalidate old
  snapshots; restoring earlier server metadata cannot revive them. Explicit refresh creates
  a new immutable catalog. A change rejects an already queued unsent call while preserving
  the active completed response.
- Unsupported modern versions cannot silently fall back to initialize. Optional sampling/
  elicitation/MRTR interactions do not invoke models or disclose state. Legacy unsupported
  requests get bounded negative control replies, flushed before EOF; notification floods
  cannot grow memory or extend deadlines indefinitely.
- Tool cancellation sends the scoped cancellation frame and then reaps. Core cancellation
  closes its journal. Legacy initialize cancellation uses transport termination without an
  illegal initialize-cancelled notification. Both protocol eras honor graceful EOF shutdown.

The old 13 process fault tests pass with MCP compiled in and in the standalone native
process profile; all existing Bash cases remain green. The three real PTY checks and
SDK/CLI JSON/RPC/Web normalized-case parity pass. Published fixtures still validate twelve
native process frames, 337 core event frames, 44 frontend RPC frames and six export entries.
Rust automated tests use a separate Rust fixture, scripted models and loopback services,
with no real credentials, remote model requests, Node or Python runtime dependency.

## Independent interoperability

[Recorded manual smokes](baselines/s6-mcp-interop.json) run the same core host example
against the [official filesystem server](https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem)
installed outside the repository. Each discovers fourteen tools, selects read_text_file,
performs one explicitly approved read of a temporary allowed fixture, checks actual returned
text and confirms exit code 0/reaping. The model is scripted; no model network call occurs.

| npm package version | SDK resolved | Observed protocol | Self-reported server version |
| --- | --- | --- | --- |
| @modelcontextprotocol/server-filesystem 2.1.0 | @modelcontextprotocol/server 2.4.0 | 2026-07-28 | 2.1.0 |
| @modelcontextprotocol/server-filesystem 2025.12.18 | @modelcontextprotocol/sdk 1.32.2 | 2025-11-25 | 0.2.0 |

The temporary development runtime is Node 24.22.2 (npm node-linux-x64); resolved package
versions and the temporary npm lock SHA-256 are retained. None is a workspace/default
dependency. These are two interoperability observations, not certification of every server,
all optional MCP capabilities, remote transports or real-model coding success.

## Scope and remaining work

This completes S6's stdio MCP tool path with approvals, budgets and trace. Host package
discovery/install/reload, stock frontend UI/command integration, remaining extension examples
and Code Mode stay open. Remote transport is deferred until there is an actual requirement.
No roots/sampling/elicitation/tasks/Apps/subscription client capability is offered. Unsupported
interactions fail with uncertainty rather than automatically replaying a tool. Input/output
schemas are retained and bounded; full JSON Schema instance validation is host-wrapper work.

Media bytes are preserved in scoped artifacts; automatic image-to-model projection,
audio/vision rendering and billed-media precision are not transport guarantees. Missing media
capture is distinct from operation failure. Result-private metadata/continuation tokens are
omitted from history. Servers are trusted host programs with user permissions, not sandboxes;
detached groups and stuck kernel calls retain the process backend's limits. Active cancellation
closes a shared connection for its queued callers. No peer annotation or metadata restores
authority, and no implicit retry recovers a completed or uncertain call. S7 remains open.

## Default footprint

[Three final-source samples](baselines/s6-mcp-report.json) use Rust/manifests/assets SHA-256
`76b06ddd6228c85dd05e42bcc73ecf89d867f180e5db04da427aab3a23d6af28`.
Revisions identify the parent because measurement precedes this commit. Cargo.lock and the
resolved package set are unchanged; base64 was already present and the MCP adapter is off
in default builds. No worker/server is started by default assembly.

| Metric | Final measurement | Change from S6 process tools |
| --- | --- | --- |
| Default resolved packages | 271 | unchanged |
| Release CLI | 16.681 MiB | +0.001% |
| Release TUI | 16.143 MiB | +0.001% |
| Release SWE-bench | 13.888 MiB | +0.001% |
| Offline smoke wall time, median | 393.03 ms | +1.54% |
| Offline smoke peak RSS, median | 7,552 KiB | -1.67% |
| Idle offline runtime RSS, median | 4,092 KiB | -0.10% |

Wall samples are 381.96 / 1041.18 / 393.03 ms. Peak RSS is 7,808 / 7,552 / 7,552 KiB;
idle RSS is 4,092 / 4,092 / 4,096 KiB. The middle outlier includes one approximately 656 ms
case; its cause is unassigned and all observations remain in the report. Small binary/RSS
differences and noisy timing do not establish an improvement or a sustained regression.
Targets/page caches are reused. Clean compilation, active MCP throughput, combined host/
server memory, real provider latency and benchmark success are not measured by this smoke.
