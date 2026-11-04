# S7 native image-generation acceptance

Native image models now generate/edit images through a dedicated OpenRouter image-modality
adapter and the shared auxiliary-task meter. SDK typed contexts and Code Mode
models.generateImages use explicit trusted routes, owned prepared inputs, ordinary approval,
parent token reservations, four shared slots, cancellation and durable task records. No nested
agent loop, arbitrary chat-model substitution, implicit retries or URL fetches are introduced.

Generated output is captured as bounded raw scoped artifacts. All images stage before any
publication; failed staging releases pending writers. Failed publication/delivery retains
known usage, incomplete-delivery state and available receipts without repeating inference.
The intentional JS representation uses image ArtifactRef blocks rather than Pi's large base64
replies. Explicit image(block) and generated-block reference editing are tested. Canonical
output/history preserves references; actual input calls/scripts are not rewritten. Optional
preview decoding validates codecs separately from raw capture completeness.

Eight new tests cover core input/capability/byte bounds and estimated counts excluding encoded
payload, native modality request/data-URI decoding, URL/base64/non-stop failures, SDK raw capture
and persisted usage, invalid output retaining reported usage, all-set staging admission failure,
missing storage/denied approval/zero budgets before HTTP, cancellation with uncertain usage,
and Code Mode generation/filtering/explicit selection/reference editing. Standalone optional
auxiliary-models tests run without JS or codecs. Existing classifier tests exercise the shared
meter after its dispatch generalization.

Actual CLI JSON, RPC, authenticated Web and TUI PTY run native generation and explicit output
under only-mode root catalogs and independent leaf approvals. Their loopback fixture checks
native modalities, normalized image delivery, canonical raw references without generated
base64 or synthetic user facts, and per-attempt usage. RPC also checks primary/combined usage.
There are eight loopback main-model and four image-task requests; no real model is called.

Pinned Rust 1.92.0 / Linux passed **30 final gates, 675 default workspace tests,
790 all-feature tests and 245 isolated core tests**. Codec-free runtime passed
32 acquisition/library, 6 Code Mode image, 5 classifier and
4 standalone image-generation checks. Commands:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked -p aicoder-runtime --no-default-features --test image_files --lib
cargo test --locked -p aicoder-runtime --no-default-features --features code-mode --test code_mode_images
cargo test --locked -p aicoder-runtime --no-default-features --features auxiliary-models --test auxiliary_models
cargo test --locked -p aicoder-runtime --no-default-features --features auxiliary-models --test image_generation
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
python3 scripts/check_file_images.py --only
python3 scripts/check_file_images.py --classifier
python3 scripts/check_file_images.py --generate-images
```

These are local results, not a hosted CI run. No actual image-generation credential, Spark/Pi
comparison, visual quality or service billing experiment was performed. Source/reference
representations and host limits are explicit in the [contract](image-generation.md).
[Migration](migrations.md) covers source-compatible ClassifierModel aliasing, new typed image
traits and profile fields, reused optional base64 and unchanged session/auxiliary record
versions. Broader hooks, expanded isolated real-model comparison and release remain open in
the [Pi audit](pi-parity-audit.md).

The [footprint report](baselines/s7-image-generation-report.json) retains all three cached
default workspace samples with source SHA-256
`8db91550a14d2d5f418af8c2874aa62ec8f6e41d7416bd2a59640d67faa634d0`.
Revision is the parent because measurement precedes commit. The package inventory remains
272; no locked package versions or default features changed.

| Workspace-default metric | Observation | Change from classifier baseline |
| --- | --- | --- |
| Release CLI | 18.047 MiB | +0.254% |
| Release TUI | 18.078 MiB | +0.257% |
| Release SWE-bench | 18.848 MiB | +0.189% |
| 125-case smoke wall, median | 390.48 ms | -1.637% |
| Smoke peak RSS, median | 8,064.00 KiB | -1.562% |
| Idle offline runtime RSS, median | 4,144.00 KiB | -1.239% |

Wall samples are **374.42 / 390.48 / 396.35 ms**, all retained.
These unisolated observations exclude native generation, active JS/codecs and rendered UIs.
They establish neither causal speed changes nor a hard RSS bound. No cold-build, allocator or
binary-section profile was performed. The distinct standalone eval command
`cargo build --locked --release -p aicoder-eval --bin aicoder-swebench` produced
**18,522,160 bytes / 17.664 MiB**
(+0.148% against the matched preceding standalone sample), with
no codec, QuickJS or auxiliary-models feature in its selected graph. It does not replace the
larger workspace-unified result or real-model quality/performance evidence.
