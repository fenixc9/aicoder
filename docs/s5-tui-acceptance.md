# S5 TUI interaction acceptance

The TUI now combines multiline/grapheme editing, bounded raw-input history, explicit captured
file pages, themes, live projected-input counts and optional declared base-rate estimates
with the existing commands/model/reasoning/branch/approval/steer/follow-up adapters. This
completes the S5 TUI item. Image input remains open; S5 and full Pi parity are not complete.

## Contract and architecture

[Interaction](tui-interaction.md) specifies input/paste/recall semantics, quotas, immutable
reference pages, palette isolation and counting/cost provenance. Core still owns Agent/
RunHandle execution, approvals, budgets, cancellation and canonical persistence. Runtime
references call the existing bounded read_file backend; they do not invent tool results,
re-read attachments on submission or install a second agent loop. Capture tasks are explicit
read-only owners, independent of the active run; cancellation/navigation discards late results.

Composer state is limited to 64 KiB, history to 100 accepted raw strings / 256 KiB, staged
file pages to four / 64 KiB, and displayed transcript previews to 512 items / 2 MiB with
16 KiB item previews. Display clipping is labelled and never claims failed archive data was
recovered. Raw canonical history remains core-owned. Recognized bracketed paste is atomic
draft data, not Enter/commands/approval answers. Untrusted terminal controls are visibly
escaped during rendering without rewriting canonical data. Shift+Enter and portable Ctrl+O
insert newlines; Alt+Enter retains follow-up semantics.

RunControlSnapshot/model_context() expose the last counted projected request without prompt
or credential data. The lightweight getter avoids copying pending input/approval payloads
for display ticks. Counts come from the same core path as budgets/policies; scope, precision,
method and fallback remain available. Input occupancy uses the lesser declared host/model
window when known; output reservations and billed usage are separate. Optional cost is a
base-rate estimate from validated declared prices and complete reported usage; unknown,
estimated, cached-price or mixed-summary work is unavailable. Browsing aggregate history
does not apply one current model price to all prior work.

Themes are frontend-only built-ins or explicit 4 KiB JSON palettes, never runtime/core
settings, model instructions, permissions or executable files. Newline/Unicode layout,
modal paste handling and file capture preserve drafts and existing owner/registry identity.

## Verification

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
cargo build --locked -p aicoder-tui
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**498 default workspace tests**, **508 all-feature tests** and **192 isolated core tests**
pass. Nine focused additions extend the prior 489/499/191 inventory:

- Grapheme edits preserve combining marks, emoji joins and CJK boundaries; multiline
  visual-row movement/wrapping does not duplicate newline rows or move outside the viewport.
- Oversized paste is rejected atomically; raw input is retained while only rendered controls
  are escaped. Input history bounds/draft restoration do not execute recalled content.
- Theme files accept only bounded known color fields; invalid/oversized data fails.
- Recognized paste cannot execute an exit command or answer approval/deletion dialogs.
- Runtime references retain immutable content, complete serialized partial pages and cursors;
  a later file change makes continuation stale. Binary pages, cancelled reads and symlink
  escape outside the workspace fail explicitly.
- An actual waiting core provider exposes counted projected metadata before responding,
  without prompt text; cancellation still produces an authoritative aborted result.
- Cost requires declared prices/complete usage; cache/incomplete cases are suppressed.
  Display cache clipping leaves usage and canonical ownership separate.
- A rendered TestBackend at the minimum terminal size keeps the multiline cursor inside
  bounds, applies the light palette, and displays terminal escape payloads as inert text.

The independent [real PTY driver](../scripts/check_tui_interaction.py) launches the binary
against a fake loopback provider and performs **11 model requests / eight turns**. It verifies
Unicode multiline input, Ctrl+P/N draft restoration, bracketed paste without automatic
submission, immutable /file capture after disk changes, live /usage details, palette switching
and terminal resizing. While generation is held, it captures another file page, steers it,
queues follow-up and checks exact subsequent user request bodies. A prepared write question
survives pasted y/newline without authorization; typed denial leaves the target absent.
Rename/root branch/model/reasoning operations preserve prior records and affect later
requests through core. Core-owner admission and metadata assertions use recorded state;
ANSI diff output is not treated as a full terminal transcript.

The prior reasoning PTY driver also passes, including read-only first-turn previews,
explicit default persistence and terminal loss during a pending provider request. The owner
journal is closed before process exit. Both drivers use Python stdlib only, fake credentials
and loopback HTTP; no remote model/billing request occurs. CI runs them after the Rust gates.
The existing 337 event frames, 44 RPC frames and six export entries still validate; persisted
session/event formats are unchanged. Rust explicit RunControlSnapshot literals add its new
optional metadata field as described in migrations.

## Scope and limits

No new package/version is resolved. TUI adds direct libc and unicode-segmentation edges,
both already present transitively; no optional vocabulary or UI dependency enters default
core/runtime. The core snapshot addition contains only bounded metadata, not frontend types.
Runtime references contain no terminal/Web type and use the shared filesystem contract.

Terminal backends may buffer a whole recognized paste event before composer admission;
64 KiB bounds accepted drafts, not arbitrary malformed/incomplete terminal protocol memory.
Legacy unbracketed text is indistinguishable from typed keys. Rendering previews and history
are not full archive exports; storage failures remain explicit and no retention guarantee is
invented. Font-dependent display cells, terminal keyboard support and synchronous backend
syscalls retain platform limits. No network price discovery, guessed cost, shell sandbox,
restored grant, image attachment or alternative execution loop is introduced.

## Measurements

[Three final-source samples and three rendered-idle samples](baselines/s5-tui-report.json)
use the pinned toolchain and the same default scripted runtime workload as
[S5 reasoning](baselines/s5-reasoning-report.json). Source SHA-256 is
`38f0b23810a5ef2d27b3f1fbe4c10731156450d17a4df5a1976d2d422d49ffa4` and includes embedded
assets. Revisions name the parent because measurement precedes this feature commit;
the digest identifies the measured sources/manifests/assets.

| Metric | Final measurement | Change from S5 reasoning |
| --- | --- | --- |
| Default resolved packages | 250 | unchanged |
| Release CLI | 15.308 MiB | -0.01% |
| Release TUI | 14.756 MiB | +0.98% |
| Release SWE-bench | 13.603 MiB | +0.03% |
| Offline smoke wall time, median | 390.96 ms | -0.15% |
| Offline smoke peak RSS, median | 7,424 KiB | +1.75% |
| Idle offline runtime RSS, median | 4,184 KiB | -1.69% |

All 25-iteration wall samples are retained: 391.09 / 390.05 / 390.96 ms. Peak RSS is
7,552 / 7,424 / 7,424 KiB; idle runtime RSS is 4,168 / 4,188 / 4,184 KiB. These local
process/GNU-time observations vary and do not establish an improvement or a proven latency
regression. Target directories/page caches are reused; clean/cold compilation, actual
provider latency, coding success and large-history rendering throughput are unmeasured.

Separately, the actual release TUI was launched in a 140x30 PTY with a fresh private session
root, no config/resources, dark palette, test model/unreachable loopback API, disabled proxy
and no provider credential or prompt. First-frame wall times are 24.37 / 29.26 / 26.82 ms.
`/proc/<pid>/status` after 200 ms reports 15,588 / 15,564 / 15,752 KiB RSS (median 15,588 KiB),
including real Tokio/HTTP/frontend assembly. Ctrl-C through clean exit takes 7.31 / 3.20 /
3.18 ms. This rendered-TUI observation is distinct from the smaller current-thread offline
runtime example and does not submit a model/coding task.
