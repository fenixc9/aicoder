# S6 Code Mode and extension acceptance

[Optional Code Mode](code-mode.md) executes actual JavaScript with QuickJS in a separate
feature-gated crate. The core owns a bounded program/intent protocol and drives every real
leaf through the existing dispatcher. CLI, TUI and Web use runtime's same registration and
settings; SDK/bench hosts can install the tool or implement another program engine directly.
The default build acquires no JavaScript engine and enables no extra tool.

## Verification

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --features code-mode --example offline_code_mode
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-codemode -p aicoder-runtime -p aicoder-web --all-features --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server,code-mode --bins --examples
python3 scripts/check_code_mode.py
python3 scripts/check_extension_ui.py
python3 scripts/check_extension_ui_browser.py
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_images.py
python3 scripts/check_web_browser.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_process_protocol.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**583 default workspace tests, 647 all-feature tests and 226 isolated core tests** pass.
Twenty engine tests and four feature-enabled runtime tests establish:

- Real Promise.allSettled, asynchronous tool promises, loops/filtering, return/text/console,
  discovery and aliases. Only selected output enters the next provider request. Syntax,
  exceptions, exit and partial output follow explicit bounded behavior.
- Script approval does not authorize a leaf. Actual owned preparation/preview commits once;
  denial reaches the JS promise as a structured rejection. A composite supplying a prepared
  operation is rejected before approval, preventing replacement of the reviewed operation.
- Parent tool/token/output/duration limits still apply. A mixed outer batch cannot spend
  quota inside composition and then execute an over-budget sibling mutation: both paths use
  atomic admission. Usage remains the sum of actual provider requests, with no nested model.
- Canonical leaf Call/Result records keep actual scope/sequence/parent linkage without
  fabricated assistant/tool transcript messages. Unsupported journals and injected call,
  result or store write failures stop admission, retain uncertainty and never retry an effect.
- Branch store copies are public bounded JSON; exception paths cannot replace the committed
  value, and switching branches does not import another branch's value or any authority.
  The offline example reads actual files twice, reopens JSONL cold between turns, recovers
  count 2 and records 12 composition facts. Both turns charge three root tool calls each.
- Cancelling/steering/revoking pending leaf approvals leaves no late grant or mutation.
  Unawaited calls are cancelled, interrupted intents remain marked abandoned and active VM
  counts return to zero. A separate deterministic cleanup pause proves steering after actual
  VM completion still blocks state commit and retains produced output. Dropped programs
  signal stop and reap independently of Tokio tasks.
- Heap, stack, wall time, interrupt checks, microtasks, owner concurrency, queues, source,
  call counts, JSON, output and public state have bounds. Admission rejects a second VM when
  full. Node/file/network/timer/import/credential bridges are absent. JSON data getters can
  reenter bounded output safely; exception formatting cannot invoke an attacker getter/tool.
- Frozen native extensions are discoverable and callable by alias. A real MCP stdio fixture
  commits its write through Code Mode using the same extension wrapper, then shuts down and
  reaps. Metadata and stored values cannot create unavailable tools or recursive composition.
- Default-off configuration, feature-free rejection, normal precedence, bounded selections
  and errors without raw input values are covered. Engine-free default trees for core,
  runtime, CLI and TUI contain no rquickjs dependency.

The real CLI JSON/RPC check executes two reads and a write: four tool calls per run including
the program. RPC asks separately for codemode and write_file with its actual prepared preview;
no file exists before the write answer. The fake provider verifies that the private large
read fields do not reach its next request. Existing terminal/browser checks, image handling
and SDK/CLI/RPC/Web normalized parity pass. The optional frontend binaries are built; the new
Code Mode interaction is exercised through CLI/RPC and SDK, while TUI/Web retain their shared
runtime path and existing UI regression coverage. No real model credentials/network are used.
Process/event/RPC/export fixtures still validate 12/337/44/6 frames/entries.

## Footprint

Final-source measurements and optional-engine observations are recorded in
[the baseline report](baselines/s6-code-mode-report.json). The report retains all samples,
source fingerprint, parent revision and limitations. The optional engine adds four locked
third-party packages (relative-path, rquickjs, rquickjs-core and rquickjs-sys) and the new
workspace crate; existing locked package versions are unchanged. The engine feature installs
no module loader, dynamic native loader or Tokio/JS futures bridge.

The three final samples share Rust/manifests/assets SHA-256
`31ba85897d477d6e78d377abff2b6b7c147f00ff447514696b1ba350c6d64388`.
Revision is the parent because collection precedes the feature commit. The default resolved
inventory is 272 packages, up from 271 solely because the empty-by-default workspace crate
is included; optional QuickJS packages are absent. Earlier pre-fix observations remain
separate in the report and are not used for final-source comparisons.

| Default metric | Observation | Change from coding examples |
| --- | --- | --- |
| Release CLI | 17.343 MiB | +1.77% |
| Release TUI | 17.310 MiB | +1.81% |
| Release SWE-bench | 14.590 MiB | +2.21% |
| 125-case smoke wall time, median | 385.19 ms | See timing comparison below |
| Smoke peak RSS, median | 8,064 KiB | +1.61% |
| Idle offline runtime RSS, median | 4,228 KiB | +2.13% |

Final wall samples are 385.19 / 391.51 / 381.62 ms. The previous primary median was
595.54 ms with documented IO outliers; its separate alternating comparison measured this
parent at median 390.55 ms. The current median is 1.37% below that observation. This is no
optimization claim or isolated causal estimate. Default help medians remain approximately
3.76–4.20 ms; none crosses the prior 20%-and-2-ms review rule. Builds reused existing targets;
clean compilation, allocator costs and OS page-cache effects are not isolated.

Separately enabling the engine yields an 18.879 MiB release CLI, adding 1,610,744 bytes
(1.536 MiB) to this default binary. The actual two-program/cold-JSONL example takes
46.53 / 870.05 / 46.70 ms and peaks at 9,728 / 9,728 / 9,856 KiB RSS. The long timing sample
is retained; its phase/cause is not localized, and no VM latency guarantee is inferred.
These active examples are different workloads from the default 125-case smoke, use tiny
programs and do not measure maximum heap use or concurrent VMs. Full-process RSS is distinct
from the configured per-VM heap/stack limit.

## S6 completion and remaining scope

The S6 implementation items now have executable paths and acceptance records:
[native hooks/providers/commands](s6-native-acceptance.md),
[process tools](s6-process-acceptance.md), [MCP](s6-mcp-acceptance.md),
[pinned resource packages/state](s6-packages-acceptance.md),
[scoped extension UI](s6-ui-acceptance.md),
[planning/protection/managed workers](s6-coding-examples-acceptance.md) and optional Code Mode.
Failures preserve canonical sessions and default applications require no script runtime.

This completes the roadmap's bounded tool composition item, not the final Pi parity audit.
Pi-specific image/classifier helpers, source pragma syntax and TypeScript ABI are absent.
The outer model still sees normal tools, so no schema-hiding/context reduction is claimed.
Stores commit after VM success; a later delivery/archive failure cannot undo committed state
or completed external effects. Bash and native tools retain host OS permissions.
S7 remains open for the full feature matrix, coding-task evaluation, identical-budget Pi
comparison, broader platform verification and release review. These offline checks and RSS/
latency observations are not a real-model coding benchmark or an OS sandbox guarantee.
