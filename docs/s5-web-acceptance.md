# S5 optional Web acceptance

The optional aicoder-web server now exposes authenticated HTTP submission, core event-v1
SSE, status, cancellation, exact prepared approvals and revocation for one host-selected
workspace. Its bundled browser page uses those routes. This completes S5's minimal Web
item and four-entry scripted consistency check; richer TUI, images and exports remain open.
S5 and the full Pi-parity roadmap are not complete.

## Contract and ownership

[The Web contract](web.md) specifies single-principal loopback authentication, fixed host
workspace/provider, monotonic prompt IDs, one admitted run, one retained latest run and two
simultaneous SSE response bodies. Core owns execution, workspace/policy checks, prepared
mutation plans, approval scopes, event sequence and journal lifecycle. HTTP/UI types remain
outside core/runtime. Observers disconnect independently; explicit cancellation, a bounded
host run budget and shutdown control the owner. Shutdown awaits execution/persistence before
closing HTTP consumers. No direct arbitrary-file, tool-call, shell or credential-routing
endpoint is provided.

Bodies, concurrent API handler work, core observation, SSE/status serialization and browser
previews are bounded independently. A preinstalled subscription retains a fast run's prefix
within core limits. Consumer overflow is explicit; status/result metadata remains separate
from complete event delivery. SSE is single-consumer without replay. No replacement terminal
frame is invented. Unexpected provider task panics produce owner_failed diagnostics and
release admission rather than leaving the run permanently busy; the bound session identity
and canonical user history remain selected for core recovery, while ephemeral authority
expires. Recorded uncertainty stays available in the session. Synchronous backend/syscall preemption is not promised.

## Verification

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo test --locked -p aicoder-web --features server
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
cargo build --locked -p aicoder-cli -p aicoder-web --features server --bins --examples
python3 scripts/check_frontend_parity.py
python3 scripts/check_web_browser.py --screenshot artifacts/web-browser.png
```

The existing **466 default workspace tests** remain green; **476 all-feature tests**
include the existing optional tokenizer tests and eight new actual-HTTP Web tests.
**187 isolated core tests** remain green. The Web tests verify:

- Missing/wrong/query-string tokens, foreign origins/hosts, unknown workspace fields and
  oversized JSON fail before any model generation; delivered assets contain no host token.
- Authenticated HTTP/SSE matches the same direct SDK final text, rounds and usage; core
  EventTraceValidator accepts complete traces. Sequential memory-session turns retain
  canonical history; reused prompt IDs and old run controls do not repeat operations.
- Prepared write previews and exact question identities gate commit/denial. Duplicate/old
  approval responses fail. Explicit session choices remain core authorization, and revoking
  a pending question prevents late authorization and leaves its target absent.
- Dropping a real SSE HTTP response does not cancel execution. Explicit HTTP cancellation
  closes the persisted journal before authoritative aborted status is published.
- Host shutdown closes admission, expires a prepared approval, leaves the mutation absent
  and awaits the journal. A real Web binary receives SIGTERM while a model request is
  pending and exits only after its stored turn is closed.
- A delayed observer overflows explicitly, while the authoritative completed result remains
  distinct. An injected provider panic produces owner_failed, retains the same session and
  both canonical user inputs, and permits a subsequent run through core recovery.

The independent [parity driver](../scripts/check_frontend_parity.py) runs real SDK, CLI JSON,
RPC and Web/SSE entry points against the same loopback response, asserting identical
`text="same answer"`, one round and reported usage `{input:1,output:1,total:2}`. It parses
machine contracts, not terminal prose, and is part of CI. This demonstrates adapter
consistency for one scripted case, not coding success or real-model performance.

The [optional browser checker](../scripts/check_web_browser.py) passed in real Chromium
151.0.7922.34 through Playwright 1.62.0: authenticate, view the prepared write, allow once,
receive SSE completion, verify the committed file, render hostile HTML as literal text
without an image DOM node, and cancel the next running turn. The check preserves CSP and
uses locator assertions rather than unsafe-eval. An isolated screenshot was reviewed;
no browser profile, token or generated screenshot is committed. Browser dependencies are
optional development tools and are not required by normal binaries or CI's parity check.
All model fixtures/credentials are fake and loopback-only; no remote model request occurs.
The existing 319 event frames and 44 RPC frames still validate, with no schema/session
version change.

## Optional cost and limitations

Root default members remain the six existing packages. Web's default feature set is empty;
`server` enables the concrete adapter. Default metadata adds only the local Web package
(250 packages versus 249), with no new default third-party package. The lockfile adds
Axum 0.8.9, axum-core 0.5.6, matchit 0.8.4 and serde_path_to_error 0.1.20 behind the optional
server feature. All-feature metadata resolves 259 packages. Core/runtime do not depend on
any of them. No Node, browser, Python, database or JavaScript engine is required by the
server/agent runtime. The page needs a browser; development browser checks need Playwright.

This remains a local single-principal integration example, not remote multi-user hosting.
It does not provide TLS, tenant routing, session archive navigation in the browser, image
uploads, replay or publication. Browser output is a bounded preview, not a full transcript
export. Unclaimed/delayed event subscriptions and network consumers can lose observation;
use authoritative status and core persisted history. HTTP header/parser/connection policy
outside authenticated handlers uses Axum/Hyper behavior; this is not a general Internet
service admission/sandbox implementation. Optional memory sessions retain their canonical
history through core's memory repository; they do not provide disk archive windowing.

## Measurements

[Three final-source default samples and three Web idle samples](baselines/s5-web-report.json)
were collected with pinned toolchain/default features and the same scripted runtime case
set as [S5 RPC](baselines/s5-rpc-report.json). Source SHA-256 is
`7d56c4760a79ed42d65716b23aacac6cd04dc28e95585499c3a187aed5cd723f`; new digests include
embedded HTML/JS/CSS. Recorded revisions name the parent because measurements precede the
feature commit; the digest identifies the measured code/manifests/assets.

| Metric | Final measurement | Change from S5 RPC |
| --- | --- | --- |
| Default packages | 250 | +1 local package, no third-party addition |
| Release CLI / TUI / SWE-bench | 14.899 / 14.452 / 13.524 MiB | all unchanged in bytes |
| Offline smoke wall time, median | 385.14 ms | -1.54% |
| Offline smoke peak RSS, median | 7,296 KiB | -1.72% |
| Idle runtime RSS, median | 4,144 KiB | +1.47% |
| Optional Web release binary | 15.142 MiB | separate opt-in executable |

Every smoke sample runs 25 iterations of five scripted cases. All wall samples are retained:
384.93 / 385.14 / 390.68 ms. Peak RSS is 7,296 / 7,424 / 7,296 KiB; idle RSS is 4,136 /
4,144 / 4,212 KiB. These local process/time-wrapper observations show variability, not a
performance improvement or a proven regression. Target directories/page caches were reused;
clean compilation, real providers, coding success and rendered-browser memory are unmeasured.

Separately launching the actual optional release Web host with no config/resources/session,
a test model, disabled proxy, unreachable loopback endpoint and no provider credential takes
23.17 / 22.94 / 25.90 ms through its listening diagnostic. First authenticated state requests
take 2.19 / 0.52 / 0.50 ms. `/proc/<pid>/status` after 200 ms reports 15,744 / 15,920 /
15,848 KiB idle RSS (median 15,848 KiB), including the real Tokio/HTTP application assembly.
SIGTERM through clean exit takes 3.19 / 3.19 / 7.28 ms. These are startup/idle observations;
no prompt/model call, browser rendering or task throughput is included.
