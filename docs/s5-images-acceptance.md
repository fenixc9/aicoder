# S5 images and frontend acceptance

S5 now accepts images through the same Agent user-message path used for text. CLI `--image`
and TUI `/image` capture bounded local files as immutable normalized JPEG; SDK hosts can use
AgentInput directly. Frontends still adapt the shared runtime/core execution loop.
See the [contract](image-input.md) and [API migration](migrations.md).

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
cargo run --locked -p aicoder-runtime --features images --example offline_images
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server --bins --examples
python3 scripts/check_images.py
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**509 default workspace tests**, **519 all-feature tests** and **198 isolated core tests**
pass. Eleven focused tests extend the prior 498/508/192 inventory:

- Core input validates user roles/blocks, image count, URLs and envelope size. Strings
  retain their existing text behavior. Image estimates explicitly identify unsupported
  visual precision and do not grow with base64 transport length.
- Stateless SDK requests, canonical session replay and controlled steer/follow-up retain
  the same image blocks. An incompatible model switch leaves the session unchanged.
  Unsupported initial/queued images cannot become applied history; rejected controls
  remain pending. Adapter validation sees the complete model projection, including the
  required host prefix, for both steer and follow-up. Known small context windows still reject
  estimated image inputs before
  writing a turn or making a model call.
- Runtime recognizes static PNG/JPEG/GIF/WebP by signature, resizes within limits,
  flattens alpha to white, applies JPEG EXIF orientation and drops original metadata.
  Source files remain unchanged; modifying them later does not alter captured input.
  Normalized data survives JSONL persistence without loading the source again.
- Runtime rejects malformed input, animated GIF, escaped paths, non-files, cancellation,
  oversized sources/dimensions/pixel counts and invalid limits. A deterministic noisy
  source exercises bounded encode attempts and resolution reduction under a 64 KiB cap.

The [image CLI/PTY driver](../scripts/check_images.py) performs **six fake model requests**:
CLI print/JSON and a real TUI image-only turn plus held-generation steer/follow-up. Its
inline images exceed 64 KiB, exercising TUI's explicit larger bounded queue admission.
It verifies JPEG data URLs, normalization notices on stderr, unchanged captured bytes
after disk replacement, independent detach, metadata-command rejection and a text-only
model's refusal without a service call. The TUI retains staged input on that refusal.

The offline SDK example verifies provider projection and canonical storage after changing
the original PNG. The prior composer and reasoning PTY drivers still pass, including
paste/approval separation and terminal-disconnect owner cleanup. The SDK/CLI JSON/RPC/Web
driver produces the same normalized scripted result through all four entry points.
The existing 337 event frames, 44 RPC frames and six export entries validate unchanged.
All services are scripted/loopback with fake credentials; no real model/billing request occurs.

## S5 audit and remaining scope

The completed stage has independent implementation and evidence for [CLI](s5-cli-acceptance.md),
[RPC](s5-rpc-acceptance.md), [Web](s5-web-acceptance.md), [canonical exports](s5-exports-acceptance.md),
[reasoning controls](s5-reasoning-acceptance.md), [TUI interaction](s5-tui-acceptance.md) and images.
S5's common scripted-case criterion passes; S6 extensions/MCP/packages and S7 final
benchmark/release work remain open. This is not a claim of complete Pi parity.

Local normalization supports static images and may reduce resolution/quality. It does not
introduce Web/RPC upload endpoints, OS bitmap clipboard integration or inline terminal
image rendering. SDK URLs remain caller-owned remote sources. Image counting is estimated,
not exact visual framing or billed usage. Codec allocation limits are partly best-effort;
source/pixel/output guards do not establish an absolute process-memory or CPU deadline.
Cancellation rejects late results but cannot preempt a decoder's bounded CPU work.
No new session/event schema version, restored authorization or implicit tool is introduced.

## Measurements and dependency cost

[Three final-source samples](baselines/s5-images-report.json) use the prior S5 default
text/tool workload and source SHA-256
`62913cc1e7747057e9760dd1e55c24fab68319afc63378773daa957955b62d60`.
The revision identifies the parent because measurement precedes the feature commit.

| Metric | Final measurement | Change from S5 TUI |
| --- | --- | --- |
| Default resolved packages | 271 | +21 |
| Release CLI | 16.523 MiB | +7.94% / 1.216 MiB |
| Release TUI | 15.993 MiB | +8.38% / 1.237 MiB |
| Release SWE-bench | 13.743 MiB | +1.03% |
| Offline smoke wall time, median | 388.74 ms | -0.57% |
| Offline smoke peak RSS, median | 7,552 KiB | +1.72% |
| Idle offline runtime RSS, median | 4,164 KiB | -0.48% |

The new packages are the optional image 0.25.10 codec tree, activated by CLI/TUI for
PNG/JPEG/GIF/WebP. Base64 was already resolved. Isolated default core/runtime trees contain
no image codec, moxcms, Rayon or tokenizer vocabulary; workspace feature unification can
enable images through its frontends. The binary cost buys local format detection,
orientation/alpha handling and bounded normalization without external imaging programs.

Wall samples are 385.75 / 388.74 / 749.58 ms; peak RSS is 7,552 / 7,552 / 7,552 KiB and
idle RSS is 4,164 / 4,164 / 4,176 KiB. These noisy local observations do not establish a
speed improvement or sustained latency regression. Targets/page caches are reused. The
first release build overlaps documentation/frontend verification;
its build seconds are not a clean or isolated compile-cost comparison. Large-image decode
throughput/peak RSS, rendered TUI idle, actual provider latency and coding success are not
measured by this text/tool smoke. Earlier rendered-TUI measurements remain historical.

The outlier review retains an additional 1512.89 / 391.84 / 1443.64 ms, exceeding the
initial 20% timing review threshold. Three subsequent alternating parent/current pairs
measured parent 384.32 / 387.18 / 401.11 ms and current 383.15 / 382.89 / 386.02 ms.
The original 749.58 ms observation included one 372 ms SessionResume case; matched runs
have SessionResume medians around 9.4–9.8 ms on both revisions. The report retains every
observation, executable fingerprints and per-case matched summaries. Parent was built
runtime-only in a detached worktree, while current is the workspace feature-unified
artifact; compile features/paths are not identical. The cause of the intermittent stalls
is unknown. This review finds no repeatable slowdown in the paired sample and makes no
latency improvement claim; stable-environment timing remains part of S7 benchmark work.
