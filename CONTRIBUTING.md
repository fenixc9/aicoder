# Contributing

The repository pins Rust 1.92.0 with rustfmt and Clippy in `rust-toolchain.toml`;
this is also the supported MSRV. The workspace uses Rust edition 2024.
Install Git, Bash, and ripgrep for the runtime and tests. Linux builds currently
also need a C toolchain, pkg-config, and OpenSSL development headers.

Run from the repository root:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo run --locked -p aicoder-runtime --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
```

Use `cargo test -p aicoder-core` (or another package name) for a focused test
run. Commit `Cargo.lock` changes when dependencies change so application builds
and CI resolve the same versions. See `AGENTS.md` for architecture and safety
conventions and `docs/evaluation.md` for evaluation workflows.
See `docs/development-baseline.md` for platform support, API/session format
baselines, and reproducible runtime measurements.

Run core separately to catch accidental dependencies on workspace-unified Tokio IO
features. Concrete file/shell tools, JSONL storage and Git verification live in runtime;
custom embedded tools/stores depend only on core. See `docs/runtime-backends.md`.
