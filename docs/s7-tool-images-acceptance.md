# S7 typed tool image acceptance

Tool/MCP images now reach compatible vision requests as bounded static previews. Canonical
sessions retain the original tool envelope and typed immutable artifact reference. Preview
bytes and synthetic user image messages are model views only. Session reordering/recovery
preserves each image with its anchoring result and recorded provenance.

Core adds the host-owned ToolImageResolver/ResolvedToolImage contract without codecs, HTTP,
new dependencies or implicit storage. Runtime's optional images feature installs a decoder
against the same explicit workspace artifact store. CLI/TUI already enable it; standalone
Web can enable its new nondefault images feature. Default dependency features and the
lockfile remain unchanged. See [contract](tool-images.md) and [migration](migrations.md).

Twelve focused new tests cover scoped immutable decoding, MIME/signature mismatch, missing
stores, incomplete/oversized references, normalization, count/inline limits, explicit
non-vision/corrupt-image fallback, timeout/cancellation/dropped resolver children, selection
without IO, tool envelope provenance, parallel wire ordering and canonical session recovery.
The actual MCP integration returns two images from a real stdio peer, inspects and decodes
the loopback HTTP JPEG parts, then reopens JSONL history and replays it without another call.
No provider-only user message, normalized bytes or private MCP metadata becomes a journal
fact. A separate four-stock-host driver checks CLI JSON, RPC, authenticated Web and a real
TUI PTY with explicit approval, successful model-image delivery and child cleanup.

The pinned Rust 1.92.0 / Linux final checks passed: **20 gates, 652 default workspace tests,
745 all-feature tests and 233 isolated core tests**. Commands:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-runtime -p aicoder-eval --features server,host-extensions,mcp-fixtures,images --bins --examples
cargo build --locked -p aicoder-web --features server,host-extensions,images
python3 scripts/check_host_extensions.py --images
python3 scripts/check_host_extensions.py --packages
python3 scripts/check_images.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_event_schema.py
python3 scripts/check_export_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
```

CI includes the new stock-host image driver; these observations establish local execution,
not a hosted CI run. All model traffic above is loopback fixture data with fake credentials.
No new real Spark/Pi requests, vision-quality result or billed-cost claim was produced.
Native local image-file acquisition/BMP, Code Mode image output/auxiliary models/only mode,
extension interception breadth and the
final larger isolated real-service comparison remain open in the [Pi audit](pi-parity-audit.md).

The source allows at most four newest tool previews per request, each at most 256 KiB of
normalized JPEG bytes (one MiB aggregate), from at most 8 MiB source per image. Older raw
artifacts remain available. Decode allocation bounds are not an OS RSS sandbox. Failed
previews leave completed tool operations and capture completeness intact. User images retain
strict input/capability checks; old JSON-only tool references remain reference-only. Older
readers reject the new image-source enum variant; journals are not rewritten for downgrade.

The [footprint record](baselines/s7-tool-images-report.json) retains three final-source
cached workspace-default samples with source SHA-256
`84e3e8568990ee65295a3940e34d7ff1780bc3205dbc9c4bbe9cfec97b7c3af8`.
Revision is the parent because measurement precedes commit. Default resolved inventory is
still 272 packages; no lockfile versions changed.

| Workspace-default metric | Observation | Change from dependency-closure baseline |
| --- | --- | --- |
| Release CLI | 17.568 MiB | +0.448% |
| Release TUI | 17.661 MiB | +0.534% |
| Release SWE-bench | 18.374 MiB | +6.270% |
| 125-case smoke wall time, median | 389.45 ms | +0.606% |
| Smoke peak RSS, median | 8,064 KiB | -1.563% |
| Idle offline runtime RSS, median | 4,152 KiB | -3.172% |

Wall samples are **394.05 / 389.45 / 384.88 ms**, all retained. No latency or memory
improvement is attributed from these unisolated samples. They exclude active codec work,
rendered frontend/server memory, cold builds and allocator/binary-section profiling.

The workspace includes CLI/TUI's enabled runtime images feature; installing the shared
resolver now keeps optional decoder code reachable from its eval builder too. To check the
standalone bench boundary separately, `cargo build --locked --release -p aicoder-eval --bin
aicoder-swebench` produced **18,060,688 bytes / 17.224 MiB**, with no image crate/features in
its selected Cargo feature tree. This is a different feature graph, not a matched historical
size comparison or a reason to discard the larger workspace observation. Core/default
standalone runtime/eval still require no codecs or image vocabulary.
