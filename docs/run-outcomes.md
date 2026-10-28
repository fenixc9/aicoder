# Run outcomes and execution budgets

`TurnExecutor::run_outcome(request, handler, context)` returns a serializable
`RunOutcome` for every normal completion, failure, cancellation or limit exit.
Every frontend and the evaluation runner uses the same `RunProgress` data;
progress is not reconstructed from UI events.

## Result contract

`RunOutcome::Completed` contains the verified final message and progress.
`RunOutcome::Interrupted` contains progress and a typed `RunStop`:

| Stop | Meaning |
| --- | --- |
| `Cancelled` | Caller/provider cancellation, with its reason |
| `Budget` | Resource, limit, consumed amount and optional next requested amount |
| `Model` | Token-limited, filtered or other non-completing model stop reason |
| `Failed` | Execution stage, redacted error message and optional typed provider error |

`RunProgress` retains the original input and validated generated messages,
usage, round/tool counts, elapsed execution time, output bytes, token-budget
accounting and the last model stop. `input_messages` separates original
conversation context from new messages. An interrupted tool batch includes
all completed results in original call order, even if another call is cancelled.
A tool that was waiting for approval/execution has no invented successful result.

`partial_response` holds accepted text/reasoning and raw tool argument fragments
from an incomplete model response. It is separate from complete messages;
fragments are not promoted into valid JSON calls. Output limits intentionally
retain only a bounded prefix. A completed tool operation whose large output
was truncated still has a recorded result marked `truncated`.

Progress can end with an assistant tool-call message and only some matching
results. Do not feed that raw unfinished unit back to a provider or automatically
repeat the missing operation. The session-aware `Agent` now saves validated tool
calls before dispatch and checkpoints completed results, including on interrupted
runs. `Session::model_messages()` supplies ephemeral error results for missing
operations and a fixed interruption marker; partial fragments remain metadata.
See [tree sessions and recovery](sessions.md) for durability and crash boundaries.

The `run`, `run_with_handler` and `run_with_context` convenience methods still
return `Result`. On interruption, their error contains an `InterruptedTurn`
context that can be retrieved with `error.downcast_ref::<InterruptedTurn>()`.
The underlying cancellation, budget or failure type remains available in the
error chain. Move result data from `result.progress`; borrowing fields such as
`result.messages` also works through `Deref`.

`Agent` adds session identity. Its structured outcomes distinguish `Completed`,
`Aborted`, `Limited`, `Stopped` and `Failed`; `progress()` returns the same core
progress for all variants. A session persistence failure retains completed
execution progress and reports stage `Persistence`. If final persistence fails after
an interruption, the primary stop remains available alongside `persistence_error`.

## Configuration

```rust,ignore
let executor = TurnExecutor::builder(provider)
    .workspace(workspace)
    .config(TurnExecutionConfig {
        max_rounds: 8,
        stream: true,
        budget: RunBudget {
            max_tool_calls: Some(20),
            max_tokens: Some(12_000),
            max_duration: Some(std::time::Duration::from_secs(60)),
            max_output_bytes: Some(1024 * 1024),
        },
    })
    .build()?;
```

All optional budgets default to unlimited; the default round limit remains 8.
A zero budget prevents the corresponding activity. Limits are independent;
the first observed stopping condition wins. Output retention remains bounded
when another budget stops first.

- **Rounds** count entered model rounds. Round exhaustion preserves complete
  history and has its own stopped state, rather than an infrastructure failure.
- **Tools** count invocations started, including unknown tools and approval
  denials. A whole batch must fit the remaining task budget and per-round cap
  before any member starts. This avoids starting only part of a mutating batch.
- **Output bytes** count model text, reasoning, tool IDs/names/arguments and
  serialized tool-result payloads before transport escaping. Control records
  and diagnostic metadata are excluded. UTF-8 prefixes never split a character.
  This bounds retained/forwarded payloads; it cannot prevent arbitrary custom
  provider/tool code from allocating its own return value before handing it over.
- **Duration** is cooperative execution time. It covers model requests/retries,
  compaction, approval, tools and verification through cancellation of their
  awaited futures. It does not preempt synchronous code or bound session I/O,
  benchmark grading, or draining a blocking host event callback. The caller's
  cancellation context is not cancelled by a child run reaching its budget.
- **Tokens** use reported totals (or reported input+output), with conservative
  estimates when measurements are unavailable. The next request must fit its
  estimated input plus at least one output token. Its output allowance is
  reduced to the remaining quota and the model's declared capacity. Deterministic
  pruning can run first without model cost. Model-backed compactors are checked
  before invocation and charged afterward, using an estimate on failure.

Token accounting is distinct from observed `ModelUsage`: unavailable usage
stays `None`, while `progress.tokens.estimated` labels the budget estimate.
Unreported retry attempts are conservatively charged estimated input plus
reserved output; another attempt is prevented when that reservation cannot
fit. Since a transport retry may not have been billed, this can stop early.
A retried request does not expose its last response's usage as a fully measured
aggregate. Estimates and third-party compactor callbacks are not a guarantee
about a remote provider's tokenizer or final bill; successful responses reporting overruns are
retained and classified as budget stops. A real failure remains the primary
stop if no budget had interrupted execution before it; conservative failure
accounting does not replace the original fault.

## CLI, TUI and bench

CLI and SWE-bench `run` accept:

```sh
--max-rounds 8 --max-tool-calls 20 --token-budget 12000 \
--time-budget-ms 60000 --output-budget-bytes 1048576
```

`--max-tokens` in SWE-bench remains a per-response setting; `--token-budget`
is the cumulative turn quota. Batch manifests include the budget settings.
TUI displays a stopped state for budget/model stops and retains usage and
streamed output. CLI exits unsuccessfully on an interrupted turn and prints
usage even on that path; unanswered stdin approval does not block runtime exit.

Evaluation reports retain `progress` and `run.stop` directly. Limited or
model-stopped runs are `Incomplete`; cancellation/provider failures remain
`Failed`, with distinct typed stop details. Earlier tool results and partial
responses are available without parsing the event trace. Completed runs are
still checked by the independent evaluators.

Regression coverage includes model/compaction/verification/approval deadlines,
parallel tool cancellation, UTF-8 retention, exact output fits, tool-output
truncation, provider failures, reported/estimated token quotas, retry accounting,
model-capacity interaction and a real CLI process with unread stdin approval.

## Verification snapshot

The [budget/outcome baseline](baselines/s1-run-budget-report.json) uses the same
125 offline cases as S0. The clean release build took 45.91 seconds; CLI/TUI/
SWE-bench binaries are 11.11 / 11.55 / 11.71 MiB, and the offline process took
302.0 ms with 4.63 MiB peak RSS. This is a diagnostic sample, not a statistical
speed claim; the source digest is recorded in the report. No initial review
threshold was crossed. The full workspace has 159 passing tests, including
19 run-limit cases, the CLI approval/process deadline test, and inherited
cancellation-reason coverage.
