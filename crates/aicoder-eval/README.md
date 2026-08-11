# aicoder-eval

Offline evaluation infrastructure for `aicoder-core`. Each run gets a temporary workspace copied
from a fixture, a complete raw event trace, deterministic post-run evaluators, and a serializable
report. Evaluation never changes the TurnExecutor or feeds grader output back into the model.

```rust,no_run
use std::path::Path;

use aicoder_core::{TurnExecutor, types::ChatCompletionRequest};
use aicoder_eval::{
    CommandEvaluator, EvalCase, EvalRunner, TrajectoryEvaluator, WorkspaceDiffEvaluator,
    WorkspaceFixture,
};
use anyhow::Result;

async fn evaluate<F>(request: ChatCompletionRequest, build_executor: F) -> Result<()>
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

Every selected case writes report.json, trace.json, and checkpoint.json. The
batch writes run.json and the official predictions.json. Re-running the same
command resumes valid checkpoints by default; pass --no-resume to run all
selected cases again.

The batch runner rejects a final answer while the Git workspace is clean and
lets the agent continue. Pass `--allow-empty-patch` only when an empty patch is
an intentional result.

Run the official installed SWE-bench Python/Docker harness separately:

~~~bash
cargo run -p aicoder-eval --bin aicoder-swebench -- grade \
  --predictions artifacts/swebench-lite-baseline/predictions.json \
  --dataset-name princeton-nlp/SWE-bench_Lite \
  --run-id baseline-v1 \
  --report-dir artifacts/swebench-lite-baseline/harness
~~~

Use aicoder-swebench import with an official report path to print a normalized
resolved rate and the original aggregate fields.
