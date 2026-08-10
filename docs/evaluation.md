# Evaluation workflow

The evaluation boundary is deliberately split into two phases:

1. aicoder-swebench run gives the agent an issue and a repository at
   base_commit, then records its patch and execution trace.
2. aicoder-swebench grade sends generated patches to the official SWE-bench
   Docker harness. Gold patches and test patches never enter the agent prompt.

## Baseline protocol

Keep these values fixed when comparing agent-loop changes:

- dataset file and selected instance IDs;
- model and model_name_or_path;
- hints policy, temperature, maximum output tokens, and maximum rounds;
- stream mode, worker count, and repository snapshot.

Start with one smoke instance, then use 20 to 50 fixed Verified or Lite
instances. Do not compare runs that selected different instance IDs.

~~~bash
cargo run -p aicoder-eval --bin aicoder-swebench -- run \
  --dataset datasets/SWE-bench_Lite.jsonl \
  --output artifacts/baseline-v1 \
  --run-id baseline-v1 \
  --model "$OPENAI_MODEL" \
  --instance-id django__django-11099 \
  --workers 1 \
  --temperature 0 \
  --max-rounds 8 \
  --max-tokens 4096
~~~

run.json is the reproducibility manifest and generation summary.
predictions.json is the official harness input. Case directories contain:

- checkpoint.json: atomically published resume state;
- report.json: outcome, usage, trajectory, workspace diff, and patch;
- trace.json: ordered raw agent events with elapsed time;
- error.json: failure chain when generation could not complete.

After generation, run the official harness and preserve its aggregate report
beside the generation artifacts. The primary metric is resolved rate; retain
generation failures, empty patches, total tokens, and duration as diagnostic
metrics rather than hiding them in the resolved-rate denominator.

The initial real-provider pipeline smoke is recorded in
[p0-smoke.json](baselines/p0-smoke.json). It is a failed generation sample,
not a resolved-rate claim: the provider stream ended during round five. The
record is retained because it verified failure classification and usage
recovery, and gives the next baseline run an explicit point of comparison.
