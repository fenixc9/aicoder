# S7 independent session fork/clone acceptance

Session forks now prepare and publish a complete selected ancestry into a distinct identity,
with fixed target IDs, raw records, source provenance and bounded editor restoration. Source and
target ownership spans native guards and observations. Staging is private and publication atomic /
no-clobber, with source revalidation and typed visible-failure evidence. See [contract](session-fork.md)
and [migration](migrations.md).

Seven core tests cover clone/before/at/root and selected-branch latest-user resolution; exact
record/target identity; no source mutation; denial/cancellation and prefix bounds without targets;
summary anchors, custom/native context and partial-turn uncertainty; off-branch labels as inert
source data with current labels/title restored; queue/authority separation; bounded editor previews;
and cancelled post-publication observers retaining both owned sessions and the completed target.
Five runtime tests exercise cold archives copying the complete prefix, real bounded target windows
and continuation; denial/window admission before publication; changed source and target collision
without overwrite or staging leaks; partitioned transaction-record bounds; and directory-sync
failure after visibility retaining a complete inspectable target. The permission fault case skips
under root; this Linux run used the ordinary owned temporary directory path.

Six executable variants cover full/window clone, full/window before-user fork, denial and failed
observers. They copy 24 or 22 fixture messages (zero on denial), check read-only preparation,
planned identity and unchanged source, and perform zero model/tool requests. Failed post-observation
has two incomplete lifecycle receipts and retains the new session. These are scripted contracts,
not coding-quality scores or billing results.

CLI tests parse actual fork/clone responses and inspect new/source files without credentials or
external network. RPC verifies one owned asynchronous fork and inert editor delivery. Web tests
verify authentication before body-driven work, busy/dedup rules, scoped UI/cancel, source-aware
continuation and owner settlement on shutdown; private data choices cannot grant tool authority.
A real Chromium browser checks clone/fork buttons, distinct identities, draft-only restoration
and explicit continuation. A real PTY checks native before-fork choice, paste remaining data,
source-byte identity, no automatic draft submission and subsequent user continuation. A TUI unit
test preserves a newer unsent draft. The optional palette placeholder regression was observed by
the PTY and corrected with completion/parsing coverage.

Linux / Rust 1.92.0 passed **117 final local gates: 872 default workspace tests,
1009 all-feature tests and 394 isolated core tests**. Standalone codec/JS-free runtime
selects 5 fork tests. Required fmt, default/all-feature Clippy, core/workspace tests,
offline smoke and strict rustdoc passed, along with previous provider/transport/tool/image/input/
message/session/frontend/schema/benchmark/SWE bridge paths. Exact commands and exits are in the
[report](baselines/s7-session-fork-report.json); these are local results, not hosted CI. There are
no new external model requests. CI includes all six examples and the real-PTY check.

Dependencies, manifests, lockfile, default features and session versions remain unchanged.
Custom backends opt into atomic fork publication; plans/backend pins are native immutable host
objects, never history authority. The fork namespace retains source IDs/selection/counts and
partial-copy metadata; ordinary continuation still validates the host route/model/window.
The pinned Pi runtime consumes before-fork cancellation and position; its unused exported
skipConversationRestore declaration is not treated as executable behavior. Tree summaries,
compaction interception/replacement and boundary context-edit/compaction drafts remain open,
as do final isolated Spark/Pi and real SWE quality/release acceptance in the [audit](pi-parity-audit.md).

Three cached workspace samples pin source SHA-256 `f7a2738daf1bb0f06bb0e76a2335b2cfeed3b708e5c295f6c7ff5daf44aba05a` and the parent
revision. All 272 resolved packages remain unchanged. Default smoke/idle
omit active forks and rendered frontends; they do not bound fork latency, copy peaks or isolated RSS.

| Workspace-default metric | Median / observation | Change from lifecycle baseline |
| --- | --- | --- |
| Release CLI | 20.794 MiB | +2.029% |
| Release TUI | 20.986 MiB | +1.133% |
| Release SWE-bench | 20.238 MiB | +0.014% |
| 125-case smoke wall | 393.97 ms | -2.347% |
| Smoke peak RSS | 9,344.00 KiB | +0.000% |
| Idle offline runtime RSS | 4,444.00 KiB | +0.725% |

Wall samples **393.97 / 393.13 / 396.25 ms**
are retained without filtering. Shared-host warm-cache observations do not establish causal speed,
exclude a smaller regression or measure cold compilation/allocator behavior. Standalone eval is
**20,036,224 bytes / 19.108 MiB** (-0.001%),
without selected codec, QuickJS or auxiliary-model dependencies. Offline measurements do not close
real-model quality or billing gates.
