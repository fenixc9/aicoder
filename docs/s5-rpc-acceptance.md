# S5 stdio RPC acceptance

The CLI now serves a versioned, bounded JSONL protocol over the same core Agent,
RunHandle and runtime assembly as other hosts. Request acknowledgement, observation and
authoritative completion are distinct. This completes the S5 RPC item. Richer TUI, image
inputs, Web and exports remain open; S5 as a whole is not complete.

## Contract and implementation

[RPC v1](rpc.md) defines monotonic JavaScript-safe request IDs, one owned run per connection,
strict command fields, run-scoped controls, approval choices, memory/disk sessions and
shutdown behavior. IDs protect operations throughout a live connection using one high-water
mark. Reconnecting does not restore the ID ledger or any authorization. Lost responses
require inspecting persisted state before choosing whether to submit another operation.

The CLI adapter introduces no model/tool loop, provider wire format, credential routing,
terminal approval reader or HTTP server. Session pages use scoped core navigation; prompt
and queued input expansion use runtime resources. Grants remain explicit, ephemeral core
host authority. Stream failure never becomes a successful-delivery claim or a synthesized
terminal event. The owner is awaited on EOF, SIGINT, shutdown and transport errors.

## Verification

Passed on the pinned Rust 1.92.0 Linux x86_64 environment:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
```

The previous 454/456 workspace tests remain, with ten real-binary RPC integrations and two
focused parser/transport tests: **466 default tests**, **468 with optional features**;
**187 isolated core tests** remain green. The additions verify:

- Acknowledgement precedes events, each trace passes core EventTraceValidator, and sequential
  memory-session turns retain canonical history without creating disk sessions.
- Failed/duplicate IDs cannot repeat operations; malformed JSON, unsupported versions,
  unknown fields, payload bounds and incompatible CLI flags are handled explicitly.
- Busy prompt admission and stale run IDs do not alter another owner; steer/follow-up inputs
  retain identity/order and pending metadata after cancellation.
- Prepared write previews, allow-session caching, revocation of pending questions, old
  approval IDs and unchanged files after denial all use core authorization decisions.
- EOF and SIGINT after a real request close the persisted journal before process exit;
  SIGINT returns 130. Closing stdout while a prepared write awaits approval cancels/awaits
  the owner, closes the journal, and leaves the target file absent.
- Nonreading stdout exits nonzero after the progress deadline; broken stdout, invalid UTF-8
  and oversized input do not leave an idle host waiting indefinitely.
- Session selection and tree/search pages reject another workspace and stale cursors.
- SDK, CLI JSON and RPC execute the same scripted provider response and produce identical
  normalized final text, rounds and usage. This proves adapter consistency for this case,
  not real-model coding success or the still-unimplemented Web path.
- Serialization bounds apply after JSON escaping; queue admission recovers after draining.

[The published fixture](protocol/rpc-v1-trace.jsonl) contains 44 frames captured from a real
loopback process, including a denied write approval, authoritative completion, status and
shutdown. Only workspace paths are anonymized. The RPC schema checker validates requests
and notifications using a local event-schema registry; the existing 319 core event frames
still validate. CI runs both checkers. No remote schema/provider or real credential is used.
The optional standard-library [Python client](../scripts/rpc_client.py) also completed a
real loopback exchange and shutdown. Python remains a development/example dependency.

## Scope and limits

The stdio backend targets the documented Unix/Linux environment. Reads/writes use separately
opened nonblocking file descriptions; synchronous filesystem/backend operations cannot be
preempted. Incoming lines are bounded to 1 MiB; core control and resource expansion limits
remain effective. Serialized frames, queue bytes/counts, observer bytes/events and stalled
write progress are bounded independently. A consumer may receive an incomplete prefix after
transport failure, with a stderr diagnostic and nonzero exit. There is no replay guarantee,
remote authentication, simultaneous multi-run scheduler or arbitrary filesystem RPC.
The CLI's fixed host workspace/provider route remains authoritative. This does not claim
Pi or JSON-RPC 2.0 wire compatibility. Core event v1 and session JSONL v3 remain unchanged.

## Footprint

[Three final-source samples](baselines/s5-rpc-report.json) use the same default features,
pinned toolchain and scripted workload as the [S5 CLI reference](baselines/s5-cli-report.json).
The source SHA-256 is `b2c19029c7fa2828bd2bf9882977f23ed0a8e19b149b7ddc15f5cb309b2b90f3`.
Recorded revisions name the parent because measurement precedes this feature commit; the
digest identifies the measured Rust sources and manifests.

| Metric | Final measurement | Change from S5 CLI |
| --- | --- | --- |
| Resolved default packages | 249 | unchanged |
| Release CLI binary | 14.899 MiB | +3.89% |
| Release TUI / SWE-bench binary | 14.452 / 13.524 MiB | unchanged |
| Offline smoke wall time, median | 391.16 ms | -0.55% |
| Offline smoke peak RSS, median | 7,424 KiB | -1.69% |
| Idle runtime RSS, median | 4,084 KiB | -0.58% |

All three 25-iteration smoke wall times are retained: 392.75 / 391.16 / 386.36 ms.
Peak RSS samples are 7,296 / 7,424 / 7,424 KiB; idle runtime RSS is 4,084 / 4,084 /
4,124 KiB. These are process/time-wrapper measurements of the existing offline runtime,
not a measure of RPC transport throughput or a performance improvement. Builds reused
the existing target directory and do not measure clean compilation. RPC adds one direct
serde dependency edge to the CLI; no resolved package version or default core/runtime
dependency is added. Provider credentials, images and optional tokenizers are not loaded
by this measurement.

The report separately retains three actual release RPC idle samples. They launch
`aicoder --rpc --no-session --no-config --no-resources --workspace <temporary-directory>`,
with a `test` model, disabled proxy, unreachable loopback endpoint and no credential.
Writing an initialize request and reading its response takes 28.40 / 27.57 / 25.50 ms;
`/proc/<pid>/status` after 200 ms reports 15,132 / 14,940 / 15,040 KiB RSS (median
15,040 KiB), including the real Tokio/HTTP application assembly. Shutdown response and
process exit take 4.56 / 3.95 / 3.83 ms. These samples do not submit a prompt, make a
provider request, render a TUI or measure a coding workload; filesystem caches are not
cleared. Idle RPC memory is distinct from the smaller current-thread offline example.
