# Local Web example (S5)

The optional aicoder-web package hosts HTTP submission, core event-v1 SSE, status,
cancellation and approval choices over core/runtime. It is a single-principal, fixed-workspace
loopback example. The normal CLI/TUI/core/runtime builds do not enable its server feature.
There is no HTTP/UI dependency in core or runtime and no separate execution loop.

## Host setup

```sh
# Supply a private random token of at least 32 printable ASCII bytes through the environment.
export AICODER_WEB_TOKEN=...
cargo run --locked -p aicoder-web --features server -- --workspace . --listen 127.0.0.1:3000
```

Provider credentials and runtime settings use the normal trusted host configuration.
The browser enters the Web token separately; it stays in page memory, never URL/cookie/
localStorage or server-rendered HTML. The server never logs or serializes that token.
The listen address must be loopback. Host and Origin checks use the actual bound address,
including a dynamically assigned port; no wildcard CORS or credential cookies are enabled.
A remote/multi-user deployment needs its own authenticated host, TLS, principal-to-workspace
mapping and lifecycle policy. This example does not offer a public server switch.

The host fixes one canonical workspace and provider route at startup. Browser input cannot
select a path, credential, provider, shell command endpoint or arbitrary tool invocation.
Prompts can request coding tools, whose preparation, workspace checks, budgets and exact
approval plans remain enforced by core/runtime. Commands run with host permissions and
are not a sandbox. The default turn time budget is 120 seconds, including approval waits.

## HTTP contract

All `/api/` routes require `Authorization: Bearer <token>` and validate origin/host before
extracting bounded JSON. Request bodies are capped at 1 MiB and authenticated handler/body work
has a ten-second timeout; API request concurrency is bounded. Unknown fields are rejected.
Startup/configuration failures are stderr diagnostics; API handler failures are structured
JSON (HTTP parser/method/extractor failures may use framework responses).

| Method / path | Input | Result |
| --- | --- | --- |
| `GET /api/state` | none | Current selected session, next prompt ID and owned run status/result |
| `POST /api/runs` | `{id,text}` | 202 acknowledgement with `version:1`, `run_id` and events URL |
| `GET /api/runs/{id}/events` | none | One SSE consumer, beginning at the preinstalled subscription |
| `GET /api/runs/{id}` | none | Owned run controls, approvals and authoritative result if finished |
| `POST /api/runs/{id}/cancel` | `{}` | Request cancellation; owner awaits terminal/persistence |
| `POST /api/runs/{id}/approval` | `{approval_id,decision}` | Answer the exact core pending question |
| `POST /api/permissions/revoke` | `{}` | Revoke selected session grants and expire pending questions |

Prompt IDs are positive JavaScript-safe integers and strictly increase across this host's
lifetime. Valid-shaped IDs are reserved even when a prompt is empty or the host is busy.
This bounds deduplication state and prevents blind retries from repeating an operation.
After a lost acknowledgement, inspect state before submitting a new prompt. IDs and run
caches are not restored after restart. Only one run is admitted at once. Later turns reuse
the session selected by the previous outcome; `--no-session` uses core memory sessions.
Only the current/latest run is retained; older run IDs fail after the next admission.

An acknowledgement is not a task-success claim. SSE `agent` events carry unmodified core
SDK envelopes; their SSE IDs are `<run_id>/<sequence>`. `observation_error` carries a core
stream error; `result` carries authoritative bounded outcome metadata after event drain.
There is no transcript duplication in result metadata or invented replacement terminal.
Unknown/old run IDs cannot control a later run. Approval choices are deny/allow_once/
allow_session; the host supplies the ephemeral scope, never the browser.

The subscription is installed before execution starts so fast completions retain their
prefix within core's bounds. Only one consumer can claim it. Reconnection/Last-Event-ID
replay is unsupported; use status for authoritative outcome and pending approvals.
If the execution task panics after binding a session, owner_failed retains that session
identity for core recovery and expires its ephemeral authority; it does not silently start
a separate conversation or fabricate a core terminal event.
Disconnect drops observation and does not cancel execution. Host budget, explicit cancel
and shutdown own that decision. A bounded slow observer can lose frames explicitly.
SSE serialization is bounded to 8 MiB per event; only two SSE response bodies may coexist
across all retained runs. Closing or completing a body releases its slot. Status output also
uses bounded serialization. Observation errors can describe core stream loss or serialization
limits; they do not fabricate delivery or retry tools.

The page displays the session/tool scope and explains that a session grant covers all
arguments for that tool. Oversized browser previews are explicitly labelled as clipped.
Revocation is a choice handled by core authority; the browser cannot submit grant rules.

## Browser and shutdown

The bundled page uses authenticated fetch streaming for SSE (no query-string token),
textContent for model/tool content, a bounded output preview, and scoped choice buttons.
Status polling recovers pending approvals/outcomes after an observation error. Raw model
HTML is never inserted. CSP, no-store and nosniff headers apply; assets contain no token.

On SIGINT/SIGTERM or server failure, the host rejects admissions, cancels and awaits the
execution owner and journal, then closes HTTP transport. A grace period bounds waiting
for network consumers after execution cleanup. Synchronous host/backend syscalls remain
non-preemptible. Dropping a router/observer is not proof that a run has stopped; embedding
hosts must explicitly await WebHost::shutdown before tearing down their executor.

Axum is enabled only by the Web `server` feature, using its
[documented SSE response](https://docs.rs/axum/0.8.9/axum/response/sse/index.html) and
[body limits](https://docs.rs/axum/0.8.9/axum/extract/struct.DefaultBodyLimit.html).
[Acceptance](s5-web-acceptance.md) records tests, optional dependencies and measurements.

Development checks can exercise all four adapters and, optionally, an isolated real browser:

```sh
cargo build --locked -p aicoder-cli -p aicoder-web --features server --bins --examples
python3 scripts/check_frontend_parity.py
python3 scripts/check_web_browser.py --binary target/debug/aicoder-web
```

The optional browser checker uses Playwright with a disposable Chromium profile and fake
loopback provider. Install scripts/requirements-browser.txt and run playwright install chromium
if needed. Python/Playwright/Chromium are development tools, not agent/server runtime dependencies. The host/provider
tests and parity driver need no remote service or real credential. Embedded callers using
`WebOptions::memory_sessions()` must also clear runtime session storage to disable artifacts.
