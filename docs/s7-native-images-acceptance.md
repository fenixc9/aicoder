# S7 native image-file acquisition and BMP acceptance

The stock read_file tool now acquires PNG/JPEG/GIF/WebP/BMP by signature through its opened
regular-file descriptor. It archives complete immutable raw bytes in explicit scoped storage;
model-only preview projection uses the existing typed artifact pipeline. Text output/cursor
formats and tool name/arguments remain compatible. Optional runtime images adds BMP to both
user-selected and tool preview decoding. No new dependency, default feature or persisted
format was introduced. See [contract](file-tools.md#native-image-files) and [migration](migrations.md).

Seven new tests cover five-format acquisition and preview decoding, codec-free raw captures,
wrong extensions/ordinary text named BMP, unchanged original bytes, source mutation/name
replacement, Unix ctime with restored mtime, workspace/symlink escapes, stale cursors, image
pagination, source/store/output admission, archive quota failure, storage timeout and dropped
writer cleanup. A complete agent loop reports a completed capture's delivery failure once;
it retains the error fact without repeating the tool. The existing codec/user-image test also
checks BMP normalization and immutable JSONL replay.

The stock-host driver exercises CLI JSON, RPC, authenticated Web and an actual TUI PTY with
an agent-selected BMP whose filename ends in .txt. The loopback server verifies the read_file
call, tool result, JPEG image parts and absence of approvals for the read-only operation.
Each canonical session contains one user input, one tool result and an artifact image;
provider-only user messages/preview notices are not journal facts. Separate text-only-model
and disabled-storage CLI cases verify explicit omission/error. All six cases retain exactly
two upstream fixture requests each; no real model or credential is used.

Pinned Rust 1.92.0 / Linux passed **21 final gates, 659 default workspace tests, 752 all-feature
tests and 233 isolated core tests**. Standalone runtime without image features additionally
passes **32 tests** (28 library + 4 native acquisition cases), demonstrating raw capture and
explicit unavailable previews without linking codecs. Commands:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked -p aicoder-runtime --no-default-features --test image_files --lib
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-runtime -p aicoder-eval --features server,host-extensions,mcp-fixtures,images --bins --examples
cargo build --locked -p aicoder-web --features server,host-extensions,images
python3 scripts/check_file_images.py
python3 scripts/check_host_extensions.py --images
python3 scripts/check_images.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_event_schema.py
python3 scripts/check_export_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
```

CI includes the new native image driver; this records local results, not a hosted CI run.
Raw input admission is at most 8 MiB, also limited by the store, with bounded excess detection
and buffered read-ahead. Complete metadata is admitted before full capture/archive. Preview
limits remain four newest images / 256 KiB JPEG each, with bounded static pixel decoding.
No-session still disables persistent artifacts; no implicit store is created. Decoder/FS
limits do not establish an OS sandbox or hard RSS bound. Raw capture retains original metadata;
normalized previews are separate. Current MIME compatibility, Code Mode output/helpers/only,
interception coverage and the larger isolated Spark/Pi comparison remain open in the [audit](pi-parity-audit.md).

The [footprint record](baselines/s7-native-images-report.json) retains three cached default
workspace samples with source SHA-256
`1791b39b7bb5b138fe94431b57e97168dc6654afd06f4e8a55a7a9d61c2e24f0`.
Revision is the parent because measurement precedes commit; default resolved inventory
remains 272 packages and lockfile versions are unchanged.

| Workspace-default metric | Observation | Change from typed tool-image baseline |
| --- | --- | --- |
| Release CLI | 17.636 MiB | +0.388% |
| Release TUI | 17.721 MiB | +0.342% |
| Release SWE-bench | 18.416 MiB | +0.229% |
| 125-case smoke wall time, median | 380.47 ms | -2.305% |
| Smoke peak RSS, median | 7,936 KiB | -1.587% |
| Idle offline runtime RSS, median | 4,156 KiB | +0.096% |

Wall samples are **379.26 / 380.47 / 384.10 ms**, all retained. These unisolated observations
exclude active image capture/decoding and rendered frontends; no runtime improvement is
attributed. No cold build, allocator or binary-section profile was performed.

The workspace eval artifact shares CLI/TUI's optional image features. The separate
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` observation remains
codec-free: **18,089,600 bytes / 17.252 MiB**, +0.160% against the preceding matched standalone
observation, with no image crate/features in its selected feature tree. This separate graph
does not replace the larger workspace observation. No real Spark/Pi or vision-quality
experiment was rerun for this feature.
