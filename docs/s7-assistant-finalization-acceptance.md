# S7 assistant finalization acceptance

Native ordered AssistantMessageHook callbacks may edit completed text/visible reasoning before
canonical acknowledgement, tool dispatch and verification. Role, actions and opaque provider
state remain exact. Paid usage is accounted before callbacks; errors retain completed originals
and storage failures never repeat provider requests. See [contract](assistant-finalization.md)
and [migration](migrations.md).

Six core integration tests cover ordered chains, one effective canonical assistant, raw preview /
final-event separation, role/size/error/panic rejection, protected tool calls, child cancellation /
timeout, unsupported and failed journals, acknowledged slow writes, paid usage, parent output
budget and final verifier input. One unit test additionally checks exact provider-state/action
protection. Two TUI tests check replacing only this response and preserving that anchor after
bounded preview eviction. A real PTY checks effective visible text, safe receipts and exactly one
persisted assistant with the raw preview absent from canonical storage.

Two runtime tests cover actual owned file effects and allow/deny in normal and optional Code Mode.
Four executable variants each perform two scripted requests, two finalizations and 14 fixture
tokens, retaining two canonical assistants. The next model request sees the edited tool-bearing
assistant; JSONL, bounded archive windows, inert HTML export and event validation pass. There are
no external model/network requests or real billing claims. Existing frontend/protocol, image,
transport, input/custom, boundary/startup, bench and SWE bridge regressions also pass. Console/Web
consume the new event; this increment does not claim a dedicated browser finalization test.

On Linux / Rust 1.92.0, **92 local final gates passed: 821 default workspace tests,
952 all-feature tests and 358 isolated core tests**. Codec/JS-free runtime selects
1 finalization test; Code Mode adds the second. Exact commands and exits are in the
[report](baselines/s7-assistant-finalization-report.json); these are local results, not hosted CI.
Required workspace fmt, default/all-feature Clippy, core/workspace tests, offline smoke and strict
rustdoc checks passed. CI now includes all four examples and the dedicated real-PTY check.

No dependencies, manifests, lockfile or default features changed. Session versions stay unchanged;
receipts use inert Extension records, with no restored callbacks or authority. The v1 event schema
adds assistant_message_finalized and explicitly rejects a User role. Streaming remains a raw
preview; this is not stream redaction. Generic User/Custom/Tool finalization, session lifecycle /
navigation veto, boundary context-edit/compaction, final isolated Spark/Pi and real SWE quality /
release acceptance remain open in the [audit](pi-parity-audit.md).

The [footprint report](baselines/s7-assistant-finalization-report.json) retains three cached
workspace-default samples with source SHA-256 `56442e5c21ea52b6927ae2a69f0a02f9ef53e19222ba06b97c02d65d5a410c0e`. The parent revision
identifies the checkout; the digest pins the uncommitted implementation at measurement time.
All 272 resolved packages and the lockfile remain unchanged.

| Workspace-default metric | Median / observation | Change from busy-custom baseline |
| --- | --- | --- |
| Release CLI | 19.867 MiB | +0.462% |
| Release TUI | 19.904 MiB | +0.546% |
| Release SWE-bench | 19.982 MiB | +0.390% |
| 125-case smoke wall | 398.77 ms | -19.203% |
| Smoke peak RSS | 8,704.00 KiB | -1.449% |
| Idle offline runtime RSS | 4,436.00 KiB | -2.634% |

Wall samples **414.09 / 394.82 / 398.77 ms**
are retained without filtering. This reference also predates the terminal-owner fix. Shared-host,
warm-cache smoke/idle omit installed finalizers and rendered frontends. They do not establish
active-hook memory, causal speed, a cold build or an isolated regression bound. The retained
[matched follow-up](s7-terminal-exit-followup.md) supplies separate prior performance evidence.
Standalone `cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produces
**19,743,864 bytes / 18.829 MiB** (+0.388%)
with no selected codec, QuickJS or auxiliary-model dependency. No real-model comparison gate is
closed by these offline measurements.
