# aicoder-eval

Offline evaluation infrastructure for `aicoder-core`. Each run gets a temporary workspace copied
from a fixture, a bounded raw event trace with explicit completeness, deterministic post-run evaluators, and a serializable
report. Evaluation never changes the TurnExecutor or feeds grader output back into the model.

```rust,no_run
use std::path::Path;

use aicoder_core::{TurnExecutor, model::ModelRequest};
use aicoder_eval::{
    CommandEvaluator, EvalCase, EvalRunner, TrajectoryEvaluator, WorkspaceDiffEvaluator,
    WorkspaceFixture,
};
use anyhow::Result;

async fn evaluate<F>(request: ModelRequest, build_executor: F) -> Result<()>
where
    F: Fn(&Path) -> Result<TurnExecutor>,
{
    let case = EvalCase::new(
        "edit-task",
        request,
        WorkspaceFixture::CopyFrom("fixtures/edit-task".into()),
    );
    let runner = EvalRunner::new()
        .evaluator(CommandEvaluator::new("tests", "cargo").args(["test", "--quiet"]))
        .evaluator(
            WorkspaceDiffEvaluator::new()
                .allow_prefix("src")
                .require_change("src/lib.rs")
                .forbid_unlisted_changes(true),
        )
        .evaluator(
            TrajectoryEvaluator::new()
                .max_rounds(8)
                .max_tool_failures(0),
        );

    let report = runner.run(&case, &build_executor).await?;
    report.write_json("eval-report.json")?;
    Ok(())
}
```

For coding tasks, the factory must explicitly install
`aicoder_runtime::tools::default_registry()?`, or use `Runtime::executor_builder_for`
for shared settings and isolated project resources. A bare core builder has no tools.
Git-based completion verification lives at
`aicoder_runtime::verification::WorkspaceChangeVerifier`; its trait remains in core.

Use `run_many` to repeat stochastic cases and obtain pass rate, mean score, and mean duration.
Subjective LLM judges remain outside the first-phase API. Deterministic online
completion gates are available through `CompletionVerifier` in `aicoder-core`.

## SWE-bench

The SWE-bench adapter loads JSON arrays, single JSON objects, or JSONL datasets,
checks out each repository at `base_commit`, and converts the issue into an
`EvalCase`. Gold patches and test patches remain evaluation-only data and are
never included in the agent prompt.

```rust,no_run
use aicoder_eval::{
    SweBenchAdapter, SweBenchDataset, SweBenchRepositorySource,
    write_swebench_predictions,
};

let dataset = SweBenchDataset::load("SWE-bench_Verified.jsonl")?;
let adapter = SweBenchAdapter::new("model-id").repository_source(
    SweBenchRepositorySource::LocalRoot("repos".into()),
);
let cases = adapter.adapt_dataset(&dataset)?;

// Run each case with EvalRunner, then export the report's workspace patch.
let prediction = adapter.prediction(&cases[0], &report)?;
write_swebench_predictions("predictions.json", &[prediction])?;
```

The generated file uses SWE-bench's official prediction fields:
`instance_id`, `model_patch`, and `model_name_or_path`. Use the official
SWE-bench Docker harness for resolved-rate grading.

### Batch runner

The aicoder-swebench binary adds filtering, a persistent bare Git cache,
bounded concurrency, per-case checkpoints, resume support, and reproducibility
metadata:

~~~bash
cargo run -p aicoder-eval --bin aicoder-swebench -- run \
  --dataset datasets/SWE-bench_Lite.jsonl \
  --output artifacts/swebench-lite-baseline \
  --run-id baseline-v1 \
  --model "$OPENAI_MODEL" \
  --limit 20 \
  --workers 2
~~~

Every admitted case writes a started marker and, when available, report/trace artifacts
plus a terminal receipt. The batch writes plan.json, run.json and one official prediction
row per selected case, including failed placeholders. Same pinned inputs can reuse valid
receipts; unsupported/uncertain evidence never causes automatic reexecution. Choose a new
output for intentional regeneration. --no-resume requires a fresh output and retains old data.

The batch runner rejects a final answer while the Git workspace is clean and
lets the agent continue. Pass `--allow-empty-patch` only when an empty patch is
an intentional result.

Install the optional pinned swebench 5.0.2 package and grade separately using the same local
dataset plus verified generation manifest:

~~~bash
cargo run -p aicoder-eval --bin aicoder-swebench -- grade \
  --predictions artifacts/swebench-lite-baseline/predictions.json \
  --dataset-name datasets/SWE-bench_Lite.jsonl \
  --generation-run artifacts/swebench-lite-baseline/run.json \
  --run-id baseline-v1 \
  --report-dir artifacts/swebench-lite-baseline/harness
~~~

Use aicoder-swebench import with a grading directory for verified linked results, or with
an official report path for its explicit total/submitted denominators. The latter alone
cannot prove a generation link.


The [S7 coding benchmark CLI](../../docs/coding-benchmarks.md) adds pinned Rust/JSON tasks,
post-run hidden graders, observed execution metadata, exclusive output archives, verification,
observation-only replay and strict comparisons. Start with:

```sh
cargo run --locked -p aicoder-eval --bin aicoder-bench -- list
mkdir -p artifacts
cargo run --locked -p aicoder-eval --bin aicoder-bench -- run --output artifacts/coding-smoke
```

Default runs are deterministic scripted contracts, not measurements of LLM coding ability.
Service calls require explicit --live. Completed but ungraded runs are now Inconclusive;
check trace_capture and workspace_capture_complete before interpreting reports as complete.


[SWE-bench schema 4](../../docs/swebench-contract.md) binds full input/receipt identities,
refuses unsafe implicit reruns, preserves failed prediction rows and joins the exact generation
scope to pinned official 5.0.2 grading. Run the offline pipeline with:

```sh
cargo build --locked -p aicoder-eval --bins --example offline_swebench
python3 scripts/check_swebench.py
```

Official Docker integration is optional and separate from model generation. Cached score
imports verify hashes and facts without launching Python/containers. Legacy submitted-only
rates are a separate diagnostic, not the full experiment denominator.
