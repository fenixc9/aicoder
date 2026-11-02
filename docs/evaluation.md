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
- completion policy (`--allow-empty-patch` is disabled by default).

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

For schema 4, plan.json pins the full selected inputs before execution; run.json is the
verified generation summary. Resume never reruns uncertain or incompatible old cases.
See [SWE-bench contracts](swebench-contract.md) for the current source/artifact pins and migration.
run.json is the reproducibility manifest and generation summary.
predictions.json is the official harness input. Case directories contain:

- checkpoint.json: atomically published resume state;
- report.json: outcome, usage, trajectory, workspace diff, and patch;
- trace.json: ordered versioned event envelopes with structured JSON payloads and producer elapsed time;
- error.json: failure chain when generation could not complete.

After generation, run the pinned official harness with --generation-run and the same local dataset export,
and preserve its aggregate report
beside the generation artifacts. The primary metric is resolved rate; retain
generation failures, empty patches, total tokens, and duration as diagnostic
metrics rather than hiding them in the resolved-rate denominator.

Generation rejects a final answer when the workspace is still clean and feeds
that observation back into the next agent round. Use `--allow-empty-patch` only
for diagnostic runs where a no-change answer is intentionally valid.

The initial real-provider pipeline smoke is recorded in
[p0-smoke.json](baselines/p0-smoke.json). It is a failed generation sample,
not a resolved-rate claim: the provider stream ended during round five. The
record is retained because it verified failure classification and usage
recovery, and gives the next baseline run an explicit point of comparison.

## Interrupted runs and quotas

The runner reads the core outcome directly. `report.json` contains full retained
`progress` and `run.stop`, including earlier tool results on interruption.
Round/tool/token/time/output limits are incomplete runs rather than generic
agent failures. CLI quota flags and accounting boundaries are documented in
[run outcomes](run-outcomes.md); batch manifests include these settings.

Provider settings are resolved once and public settings are included in the run
manifest; workers share the provider/HTTP pool. Sampling is unset by default,
so choose supported settings explicitly when comparing runs. See
[provider configuration](provider-configuration.md), especially DeepSeek thinking
mode's history and unsupported-parameter behavior.

Trace consumers should validate [event protocol v1](events.md). Incomplete observation
is an evaluator error; old Debug-string traces are historical artifacts, and SWE-bench
legacy/incompatible checkpoints are refused without automatic reexecution. Use a fresh
output for an intentional new attempt; do not mix old results into current evidence.


## S7 coding cases and verifiable archives

[aicoder-bench](coding-benchmarks.md) adds a small Rust/JSON task suite over the same EvalRunner
and core/runtime API, with an offline default, explicit service opt-in, observed input pins,
independent post-run graders, immutable artifact references, validation, event replay and
strict comparison. Every selected setup/provider/budget/grader/artifact/cancellation outcome
stays in the denominator. This does not replace the official SWE-bench resolved-rate workflow.
EvalRunner now reports completed but ungraded runs as Inconclusive, bounds trace/workspace
retention, and preserves progress when post-run observation fails. Legacy SWE-bench generation
completion and official test resolution remain different outcomes; see the migration notes.


Official grading targets the published swebench 5.0.2 wheel/content hash, not mutable upstream
main. Failed generation receives an explicit empty prediction and remains selected. Import
resolved_rate uses official total_instances, with submitted_resolved_rate separate. Grading
cache identity includes the exact staged prediction/dataset and a fresh private official run
nonce. See [the S7 contract](swebench-contract.md) and [acceptance](s7-swe-acceptance.md).
