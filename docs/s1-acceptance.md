# S1 acceptance

> S3 relocation: current coding smoke is `cargo run --locked -p aicoder-runtime --example offline_smoke`;
> concrete backends now live in runtime. Commands/measurements below record this historical increment.
> See [backend migration](runtime-backends.md).

S1 is complete. Its scope is the reusable model/execution boundary and the
first compatible HTTP adapter, not the remaining Pi user-facing features.

| Requirement | Implementation/evidence |
| --- | --- |
| Neutral messages, content blocks, model metadata, typed errors and optional usage | [model contract](../crates/aicoder-core/src/model.rs), [contract tests](../crates/aicoder-core/tests/model_contract.rs) |
| HTTP and wire protocol outside core | [providers crate](../crates/aicoder-providers/src/lib.rs); core normal dependency tree has no Reqwest/Hyper/TLS or terminal UI packages |
| Endpoint, credential, proxy, timeout and compatibility settings | [profile resolver](../crates/aicoder-providers/src/config.rs), [credential sources](../crates/aicoder-providers/src/credentials.rs), [configuration guide](provider-configuration.md) |
| Model registration and injection | Named provider IDs, model registry and `ModelProvider`; [public SDK tests](../crates/aicoder-core/tests/public_api.rs), profile and full HTTP tool-loop tests in providers |
| Unified stream/non-stream results | Existing SSE boundary/tool-fragment tests plus cancellation, size ceilings and reasoning-history tests in [provider tests](../crates/aicoder-providers/src/chat.rs) |
| Partial outcomes and quotas | [run contract](run-outcomes.md), [run-limit tests](../crates/aicoder-core/tests/run_limits.rs), [CLI process tests](../crates/aicoder-cli/tests/run_budget.rs) |
| In-memory model/tool/store embedding | [embedded agent](../crates/aicoder-core/examples/embedded_agent.rs), [direct model provider](../crates/aicoder-core/examples/model_provider.rs) |
| Existing coding workflow remains runnable | [offline smoke](../crates/aicoder-runtime/examples/offline_smoke.rs), [runtime tool tests](../crates/aicoder-runtime/src/tools/tests.rs), [evaluation tests](../crates/aicoder-eval/tests/eval_runner.rs) |

Verification on the pinned Rust 1.92.0/Linux toolchain:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo run --locked -p aicoder-core --example offline_smoke
cargo run --locked -p aicoder-core --example embedded_agent
cargo run --locked -p aicoder-core --example model_provider
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
```

183 tests pass. Transport tests use local fake HTTP/proxy endpoints and isolated
child-process environments, not paid model requests or global environment mutation.
The dependency lock drops 27 unused package versions (261 to 234), including
Hickory/Moka. This does not claim a measured coding-task success rate.

The adjacent shell fix ensures command stdout/stderr reaches the model and
managed provider credentials are excluded from inherited child environment.
Shell environment allowlisting and bounded subprocess I/O remain S3 work.

S2 starts next: tree sessions and migration, interrupted-turn persistence,
run control (steer/follow-up), versioned events, backpressure and disconnect
semantics. Configuration files/project instruction discovery remain S3;
long-session summaries remain S4; complete frontend/RPC/Web workflows remain
S5; extension/MCP/package features remain S6. Broader bench/Pi parity acceptance
remains S7. These are deliberately not claimed complete by the S1 gates.

The [final provider baseline](baselines/s1-provider-report.json) records a
43.70-second clean release build and 125 passing offline cases. CLI/TUI/
SWE-bench binaries are 9.33 / 9.78 / 9.95 MiB, about 10–12.5% below S0.
The offline runtime sample is 313.1 ms with 4.75 MiB peak RSS. The measured
source digest is verified against the worktree; timings are diagnostic,
not a statistically significant speed or coding-success claim.
