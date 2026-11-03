# S7 explicit Code Mode image output acceptance

Code Mode now supports await image() for supplied inline base64 and scoped image references.
Tool functions add .result(args) for the complete host envelope; normal calls keep their
previous output-value contract. The VM emits a core ImageCall intent to a host-registered
read-only tool. Runtime opt-in installs image_output and its existing bounded store binding.
The VM acquires no filesystem/network/storage client. Core builders still install no tools.

Image calls follow the ordinary frozen catalog, guards, parent tool budgets, cancellation and
canonical Call/Result journal. Four explicit intents are admitted per program and selected
references retain intent order. Reading an image without selecting it retains the canonical
leaf image but excludes its pixels from the outer model view. Non-image recovery archives
retain their outer references. Completed selected images remain partial output after a later
script exception; cancellation/dropped futures stop writers and VM owners without late model
requests. Existing historical image blocks are not rewritten.

Eight new tests cover the public core contract without a JS engine/codecs/HTTP, ambiguous
single-image results, filtered canonical leaf history, selected inline/reference views,
non-vision and codec-free omission, normal/result promise contracts, exact allowlists,
read-only capability enforcement, four-intent and parent-budget limits, tool guards,
foreign storage, bad source data, cancellation and dropped VM/writer ownership. The existing
runtime/native/MCP image contracts continue to verify actual codec/source limits. All tested
source operations remain under the existing host storage and tool authority.

The actual CLI JSON, RPC, authenticated Web and TUI PTY driver runs a real QuickJS program
with read_file.result() and image(). It verifies one outer tool pair, canonical image references,
JPEG image parts in the loopback model request and source files left unchanged. Separate
text-only, no-storage and filtered-output cases retain all outcomes. Each of seven cases uses
two upstream fixture requests, fourteen total. Missing storage and unselected media never
become fabricated successful image delivery or synthetic user history.

Pinned Rust 1.92.0 / Linux passed **25 final gates, 666 default workspace tests, 766 all-feature
tests and 238 isolated core tests**. Standalone codec-free runtime additionally passes its
32 existing library/acquisition tests and six Code Mode image tests under the code-mode
feature without image codecs. Required and feature-specific commands:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked -p aicoder-runtime --no-default-features --test image_files --lib
cargo test --locked -p aicoder-runtime --no-default-features --features code-mode --test code_mode_images
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-runtime -p aicoder-eval --features server,host-extensions,mcp-fixtures,images --bins --examples
cargo build --locked -p aicoder-web --features server,host-extensions,images
python3 scripts/check_file_images.py
python3 scripts/check_host_extensions.py --images
python3 scripts/check_host_extensions.py --images --mime-alias
python3 scripts/check_images.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_event_schema.py
python3 scripts/check_export_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web --features server,images,code-mode --bins
python3 scripts/check_file_images.py --code-mode
```

CI includes the new opt-in stock-host driver. These observations establish local execution,
not a hosted CI run. No real Spark/Pi or vision-quality experiment was performed. Existing
static preview limits remain four newest images / 256 KiB each; raw admission is at most
8 MiB and further bounded by storage. Default leaf argument JSON still caps inline image
transfer at 64 KiB, so large captured media uses references. Capture/selection completeness,
raw source completeness and model delivery remain separate. Backend integrity checks and
codec allocation requests do not constitute a hard RSS or OS sandbox guarantee.

ProgramEvent::ImageCall extends the public Rust enum and CodeModeConfig adds the optional
host image tool name. Session/composition schemas remain compatible; a migration note covers
exhaustive matches and the reserved runtime tool name. Reused base64 is enabled only by the
optional runtime code-mode feature, with no new dependency/default feature. Explicit subsets
must allow image_output; script state or a stored ID cannot select storage or grant authority.
Classifier/image-model helpers, only mode, broader interception and final isolated evaluation
remain open in the [Pi audit](pi-parity-audit.md). See the [API contract](code-mode.md#explicit-images).

The [footprint record](baselines/s7-code-images-report.json) retains three cached default
workspace samples with source SHA-256
`032ca81d123059e50ce96cfc6d610f40b90e6019acfbfa0f5a4eaf614df90998`.
Revision is the parent because measurement precedes commit. Default resolved inventory remains
272 packages; lockfile versions are unchanged.

| Workspace-default metric | Observation | Change from MIME baseline |
| --- | --- | --- |
| Release CLI | 17.689 MiB | +0.116% |
| Release TUI | 17.775 MiB | +0.113% |
| Release SWE-bench | 18.472 MiB | +0.104% |
| 125-case smoke wall time, median | 380.71 ms | -3.664% |
| Smoke peak RSS, median | 7,936 KiB | -1.587% |
| Idle offline runtime RSS, median | 4,064 KiB | +0.594% |

Wall samples are **392.70 / 380.71 / 380.30 ms**, all retained. These unisolated default
observations exclude active JS workers, image capture/decoding and rendered frontends;
no causal latency/memory improvement is attributed. No cold build, allocator or binary-section
profile was performed. The separately selected standalone eval graph has no image or rquickjs
dependency: `cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**18,117,920 bytes / 17.279 MiB**, +0.112% against the preceding matched standalone observation.
It does not replace the larger feature-unified workspace result or measure active optional VMs.
