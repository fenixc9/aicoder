# S5 reasoning controls acceptance

Core now exposes typed reasoning effort, explicit per-model capability declarations,
validated atomic model/effort selection and branch-local durable preferences. Runtime
registers trusted model metadata and layered options; CLI/TUI expose working controls.
This completes the reasoning-control portion of S5. Richer TUI editing/history/references/
themes/context/cost and image input remain open; S5 and full Pi parity are not complete.

## Contract and architecture

[The reasoning contract](reasoning-controls.md) distinguishes omitted service default,
explicit none, declared controls, visible reasoning history and requested versus billed work.
Unsupported choices fail during core/adapter preflight before user/turn or selection writes.
Generic registered reasoning models use reasoning_effort plus max_completion_tokens;
classic defaults retain their old wire shape. Existing DeepSeek compatibility profiles
retain their fixed enabled/high and disabled contracts. No model-name support inference,
model discovery or actual provider availability claim is introduced. Protocol mapping was
checked against [OpenAI Docs](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)
on 2026-10-30; model-specific support remains explicit host metadata.

Reasoning choices are small aicoder.reasoning.v1 extensions tied to provider/model epochs.
Branch checkout restores that branch's preference; selecting another model does not reuse
an older epoch's value. An explicit combined model/effort API commits one validated batch.
Known metadata is retained in bounded archive indexes/windows; it never becomes model text,
private backend state, a credential route or restored authorization. Canonical history and
projection/summary repairs remain separate. Summary generation retains its own bounded,
tool-free policy/defaults and attempt charging. Coding effort does not raise run budgets.

Agent::with_reasoning_effort forks request options over the same live executor/registry;
it does not rebuild tool identity or recreate grants from history. Model metadata arrays
are limited to 128 per trusted profile and existing settings byte limits. Project settings
cannot define those profiles/routing/storage. Explicit host preference provenance is separate
from inherited branch metadata. CLI/RPC/Web explicit configuration overrides saved choices;
TUI treats configured values as initial fallback and validates interactive changes.

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
cargo run --locked -p aicoder-runtime --example offline_reasoning
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
cargo build --locked -p aicoder-tui
python3 scripts/check_reasoning_tui.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**489 default workspace tests**, **499 all-feature tests**, and **191 isolated core tests**
pass. Twelve focused Rust additions extend the prior 477/487/187 baseline:

- Four isolated core tests distinguish omission/none, reject duplicate or unsupported
  declarations, validate branch-local choices, atomically select model/effort, reset epochs,
  retain old branches, preserve explicit default across a later fallback, reject malformed
  metadata and cancel without writes. An explicit model selection can rebind an old route's
  metadata through the host's current provider without routing credentials from history.
- Three provider tests capture actual loopback request bodies for omitted/none/low/max,
  enforce modern versus legacy output-cap field names, and retain fixed DeepSeek wire
  contracts. A credential provider that panics if called proves unsupported requests do not
  even acquire credentials; no real model or remote service is involved.
- Three runtime tests verify trusted profile metadata/nullable precedence without keys,
  reject project-defined profiles, retain effort through cold archive windows without
  inserting metadata into model text, and execute two approved writes over a preference fork
  and its base using the same live registry. Only the first needs session approval.
- One real CLI test selects/clears a stored preference with no credential and proves an
  unsupported choice leaves canonical bytes unchanged and stdout empty.
- One TUI state test verifies validated first-turn staging, draft preservation, rejection,
  navigation reset and cancellation without creating chat turns or leaking pending choices.

The independent [PTY driver](../scripts/check_reasoning_tui.py) additionally launches the
real TUI against a fake loopback provider. It verifies first-turn low selection without any
session/model write during preview; explicit default survives subsequent turns even with
startup high fallback; unsupported max leaves later choices intact; and exact requests use
max_completion_tokens. Captured efforts are low/default/default/default/default/high.
The first process exits cleanly through Ctrl-C. A second process loses its terminal while
an actual model request is pending; the TUI cancels/awaits the owner and persists the sixth
terminal turn before exiting. This uses only Python stdlib, no browser or real credential.
CI runs this driver and the offline example.

The [offline example](../crates/aicoder-runtime/examples/offline_reasoning.rs) generates
high then low, validates a complete 18-frame core trace and demonstrates epoch reset without
network access. Its [recorded fixture](protocol/reasoning-v1-trace.jsonl) passes event v1
validation; the default corpus is now 337 event frames. Existing 44 RPC frames and six
canonical export entries still validate. Event v1 gains only optional requested-effort
context metadata; session JSONL remains v3. Updated schemas are required by strict external
validators. Rust explicit field literals need the documented additions/defaults.

## Scope and cost

No new dependency/package version, tokenizer vocabulary or native scripting runtime is
introduced. Core remains independent of HTTP/runtime/frontends. Metadata-only controls
perform no model call or tool dispatch; generation still uses the shared execution loop,
approval, budgets, cancellation, persistence and event contracts. Requested effort is not a
billing/quality/latency guarantee. Trusted metadata can be wrong about a remote endpoint;
provider failures remain explicit. Summary effort is separate from a coding turn preference.

This does not implement a new provider protocol, remote discovery, reasoning mode/pro service,
TUI multiline editor, image input or a universal list of model-supported settings. CLI/RPC/Web
can configure effort through normal host settings/startup flags; RPC/Web do not add a browser/
wire command for arbitrary model parameter changes. Optional SDK APIs allow other hosts to
adapt them. TUI EOF/error cleanup awaits core owners, but synchronous backend syscalls remain
non-preemptible. Changes to metadata are scoped to the existing workspace/run lease and do
not silently restore or extend ephemeral host authority.

## Measurements

[Three final-source samples and three reasoning-case samples](baselines/s5-reasoning-report.json)
use the same pinned toolchain/default scripted runtime workload as
[S5 exports](baselines/s5-exports-report.json). Source SHA-256 is
`678f9734f547df522f5cc1c60825604a48586fa61d77b4fc6244e579f7e380b5` and includes embedded
HTML/JS/CSS. Recorded revisions name the parent because measurement precedes this commit;
the digest identifies the measured code/manifests/assets.

| Metric | Final measurement | Change from S5 exports |
| --- | --- | --- |
| Default resolved packages | 250 | unchanged |
| Release CLI | 15.309 MiB | +1.06% |
| Release TUI | 14.613 MiB | +1.17% |
| Release SWE-bench | 13.600 MiB | +0.60% |
| Offline smoke wall time, median | 391.55 ms | +0.18% |
| Offline smoke peak RSS, median | 7,296 KiB | -1.72% |
| Idle runtime RSS, median | 4,256 KiB | +3.40% |

All 25-iteration smoke wall samples are retained: 366.34 / 391.55 / 391.82 ms. Peak RSS
is 7,296 / 7,680 / 7,296 KiB; idle RSS is 4,268 / 4,256 / 4,184 KiB. These local
process/time-wrapper observations vary; they do not establish an improvement or a proven
latency regression. Target directories/page caches are reused, not clean/cold builds.
Default core/runtime builds gain no package or tokenizer dependency.

The actual release offline_reasoning case performs two scripted coding requests (high/low),
metadata validation/commits, complete 18-frame trace checking and epoch reset. Wall samples
are 17.36 / 19.63 / 17.46 ms (median 17.46 ms), with 5,632 KiB peak RSS in all three.
These include process/GNU-time, temporary session IO and sync costs. They do not measure
real-model effort, billed work, quality, TUI rendering memory or remote API latency.
