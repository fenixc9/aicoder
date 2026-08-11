# Context compaction

Context compaction is an opt-in `TurnExecutor` policy. It runs before a model
request when the estimated input plus the reserved completion capacity would
exceed the configured model context window.

## First policy

`PruningContextCompactor` is deterministic and provider-independent:

- System messages are retained.
- The newest complete message unit is retained, followed by as many recent
  units as fit the configured preservation budget.
- An assistant tool-call message and its following tool results are one atomic
  unit and are never split.
- Removed history is replaced by a fixed marker. Raw user or tool content is
  not promoted into a generated system summary.
- The model-facing context is compacted, while `TurnExecutionResult.messages`
  remains the complete transcript used by session persistence and auditing.

The built-in estimate is deliberately conservative for non-ASCII text. Exact
token counts are model-specific, so the configured limit should retain normal
provider headroom.

```rust
use aicoder_core::{
    ContextWindowConfig, PruningContextCompactor, TurnExecutor,
};

let compactor = PruningContextCompactor::new(ContextWindowConfig {
    max_context_tokens: 64_000,
    reserved_output_tokens: 4_096,
    preserve_recent_tokens: 8_192,
})?;

let executor = TurnExecutor::builder(provider)
    .workspace(workspace)
    .context_compactor(compactor)
    .build()?;
```

The CLI exposes the same policy with `--context-window TOKENS`. It remains off
by default because a model name does not reliably identify its deployed context
limit.

## Extension boundary

`ContextCompactor` is asynchronous so a later semantic implementation can call
a summarization model. Its result includes compaction-call usage, which the
executor adds to total usage. A semantic compactor must still satisfy these
invariants:

1. Return a request below `target_tokens`.
2. Reduce the estimated context size.
3. Preserve current instructions and complete tool-call units.
4. Treat tool output as untrusted data when producing a summary.

Compaction emits state transitions and started, completed, or failed events.
Evaluation traces aggregate compaction count, failures, and removed messages.
