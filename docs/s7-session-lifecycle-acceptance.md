# S7 session adoption/navigation and lifecycle acceptance

Native SessionHook guards inspect read-only plans with fixed targets, IDs and transactions under
leases. Guarded branch/checkout validates the projected model context before selection. Before
callbacks can veto; completion observations report delivery separately and cannot repeat effects.
See [contract](session-lifecycle.md), [migration](migrations.md) and [remaining audit](pi-parity-audit.md).

Twelve core tests cover plan immutability/label resolution; actual adopted metadata; denial with
no writes/after notifications; cancellation/timeout and private child cleanup; foreign/busy sources;
post-commit cancellation and retained ownership; observer error redaction; explicit startup/new/
resume/shutdown and same-binding no-op; hosted veto before models or turns; acknowledged creation
when the duration expires; and explicit session authority retired at commit before observations,
with denial leaving it intact. Three runtime tests exercise cold JSONL windows, real history,
read-only preparation and exact marker IDs; byte preservation on denial; and source changes during
guards that fail without refreshing the plan or appending a retargeted transaction.

Five executable variants use full/window views, allowed/denied rename and failed observers with
real JSONL storage and **zero model/tool executions**. Planned branching and unchanged source-byte
prefixes are checked separately. A real PTY verifies startup without a user turn, a private guard
choice before file writes, paste as data, rejected rename with byte identity, and an allowed owned
rename. The transcript retains one User and one Assistant. The driver uses the declared /name
command and verifies canonical records, guard options and file bytes, independently of literal
differential-render output.

TUI uses asynchronous navigation/new/resume/deletion tasks; rejected changes retain view/draft and
successful changes retire delegated authority before observations. RPC has a bounded owned selection
task, remains responsive to status/targeted cancel/shutdown, and awaits ownership after EOF/failure.
Its unit regression verifies those controls without model IO. Web tests prove authenticated veto
before model execution and one startup per binding / one shutdown after owner settlement, including
repeated shutdown calls. Hosted session preparation uses the existing agent loop and parent clock.
CLI metadata commands still work without external network or model credentials. New RPC schema
cases validate safe cancellation IDs; result observations carry no private routes or restored grants.

Linux / Rust 1.92.0 passed **107 local final gates: 857 default workspace tests,
992 all-feature tests and 387 isolated core tests**. The standalone codec/JS-free runtime
suite selects 3 lifecycle tests. Exact commands and exits are in the
[report](baselines/s7-session-lifecycle-report.json). Required workspace formatting, default/all-feature
Clippy, core/workspace tests, offline smoke and strict rustdoc passed. Existing message/input/custom,
transport/images/frontends, schemas, benchmarks and SWE bridge checks also passed. These are local
results, not hosted CI. CI now includes all five examples and the real-PTY check.

No dependencies, manifests, lockfile, default features or session versions changed. Custom window
backends opt into prepared navigation; old low-level navigation remains a trusted host primitive.
Lifecycle positions/reports are inert metadata; callbacks, leases, backend previews, provider routes,
queues and tool grants never restore from history. This increment does not close cross-session
fork/clone, tree-summary customization, before-compaction replacement or boundary context-edit /
compaction drafts. Final isolated Spark/Pi and real SWE quality/release acceptance remain open.

Three cached workspace samples pin source SHA-256 `0725e1ce3bbb18d7b8c0ba290ab43e1eb164035213cc2056f4db626d8c74d42d` and the parent
revision. All 272 resolved packages are unchanged. Default smoke/idle omit
installed session hooks and rendered frontends; they are not an active-hook or isolated RSS bound.

| Workspace-default metric | Median / observation | Change from general-message baseline |
| --- | --- | --- |
| Release CLI | 20.380 MiB | +1.014% |
| Release TUI | 20.751 MiB | +2.541% |
| Release SWE-bench | 20.236 MiB | +0.132% |
| 125-case smoke wall | 403.44 ms | -58.827% |
| Smoke peak RSS | 9,344.00 KiB | +4.286% |
| Idle offline runtime RSS | 4,412.00 KiB | +1.007% |

Wall samples **403.44 / 393.62 / 405.29 ms**
are retained without filtering. Shared-host warm-cache observations do not prove causal speed,
exclude a smaller regression, or measure cold-build/allocator behavior. The prior
[independent-source comparison](baselines/s7-message-finalization-matched-report.json) is separate
historical evidence, not a measurement of this increment. Standalone eval release is
**20,036,432 bytes / 19.108 MiB** (+0.241%),
without selected codec, QuickJS or auxiliary-model dependencies. No real-model quality or billing
claim follows from these offline observations.
