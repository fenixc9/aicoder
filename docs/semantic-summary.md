# Semantic context summaries (S4)

Automatic and manual summaries share `SemanticContextCompactor` in core. The default
runtime strategy remains deterministic pruning. Semantic summaries add no dependency,
HTTP client, terminal type, implicit tools or provider registration to core.

## Host API and configuration

SDK hosts install a compactor with an injected `Arc<dyn ModelProvider>`, model and
`SummaryConfig`. The summary model may differ from the coding model. Default assembly
uses the configured coding provider/model. `Agent::compact_session(repository, id, context)`
returns a typed `ManualCompactionReport`; it holds the same exclusive session lease as a
coding turn and neither creates a fake prompt nor executes tools. Manual calls require
an installed compactor with `persistent_summaries() == true` and the matching workspace.
A plain SDK executor can use summaries transiently with the default no-op journal.
Custom journals implement `record_summary` to acknowledge a durable checkpoint before the
next model call. Its indices refer to canonical history, not the reduced model view.

Runtime settings support the existing field-by-field precedence:

```json
{
  "version": 1,
  "settings": {
    "context": {
      "strategy": "semantic",
      "window_tokens": 32768,
      "reserved_output_tokens": 4096,
      "preserve_recent_tokens": 6000,
      "summary_output_tokens": 1024
    }
  }
}
```

Environment equivalents are `AICODER_CONTEXT_STRATEGY=semantic`,
`AICODER_CONTEXT_WINDOW=32768`, and `AICODER_SUMMARY_OUTPUT_TOKENS=1024`.
`strategy` accepts `prune` or `semantic`; semantic mode requires an explicit window.
Summary output capacity must be 1–8192 tokens. The actual output limit is also bounded
by model metadata and remaining run budget. Runtime limits each summary input to the
configured window minus summary output, clamped to 512–32768 estimated tokens and further
restricted by provider context metadata. Other SDK policy limits remain configurable.

CLI: `aicoder --context-window 32768 compact-session SESSION_ID` prints the structured
report and fails the command if compaction fails. TUI: `/compact` summarizes the selected
stored session while idle; the normal cancellation control applies. Both use
`Runtime::summary_agent`, which chooses semantic mode explicitly even if automatic
strategy remains `prune`. A known context window and provider credentials are required
for these online commands. They do not change the user's saved settings.

## Reduction and resource limits

Automatic reduction triggers only above the context input target. The protected runtime
system prefix and newest complete message unit stay intact. Additional recent units are
retained within the configured recent target (at most half of input capacity). Retained
messages must be an exact suffix of canonical history; a tool result cannot start that
suffix. If this boundary or the resulting summary cannot fit, failure is explicit.

Older units are sent in bounded chunks with the previous structured summary merged into
each subsequent chunk. The prompt requests five string arrays: goals, constraints,
changes, todos and notes. Results must be valid, nonempty JSON with no extra fields,
at most 64 strings per field, 4096 bytes per string and 32 KiB overall. Responses must
finish normally and contain no tool calls. The coding request contains a fixed User-role
wrapper around that JSON; model-generated prose is never promoted to a system instruction.
Summary requests have no tools and use `tool_choice: none`.

Default SDK policy allows two attempts per chunk, eight calls overall, 1024 output tokens
and 8192 estimated input tokens. Constructor bounds are three attempts, 32 calls,
8192 output tokens and 131072 estimated input tokens. Source serialization is capped at
2 MiB before cloning the sanitized summary source; size checking and source hashing stream
to bounded/sink writers. Chunk selection uses additive conservative estimates instead of
repeatedly serializing an increasingly large payload. Images become fixed omission notices;
reasoning and opaque provider state are omitted. Their original blocks remain in history.

Failed automatic summaries fall back to deterministic pruning by default. SDK hosts may
select `SummaryFailurePolicy::Stop`. Cancellation and token-budget exhaustion always stop;
manual compaction reports failure instead of silently pruning. A bounded in-memory cache
remembers the last 16 failed source/model fingerprints, so an unchanged source is not
immediately retried by the same compactor. Manual calls bypass this cache. A successful
durable summary is reused when its branch resumes. The failure cache is not persisted
across fresh processes, and growing history may legitimately require another summary.

## Durability, branches and uncertainty

JSONL remains version 3. The reserved extension `aicoder.context.summary.v1` records the
structured draft, generator identity, attempt/usage accounting, source leaf and revision,
a 64-bit source fingerprint, first retained entry ID and retained/replaced message counts.
The fingerprint is a diagnostic of the projected source, not a cryptographic integrity
proof. Raw recorded messages and inactive branches are retained unchanged.

The repository validates the checkpoint transaction before committing it: source leaf
must be its direct parent, the retained anchor must be an ancestor message beginning a
complete unit, counts must agree, and automatic accounting must name the active turn.
The latest valid checkpoint on an ancestry supplies its model context. Selecting an older
leaf or sibling excludes checkpoints outside that branch. Legacy `Compaction` records and
unrecognized extensions remain metadata; their text is not activated automatically.

Interruption notices and synthetic missing results remain context-only. A fixed recovery
notice accompanies summaries of uncertain operations, including after restore. Pruning
fallback preserves the original interruption markers. The summary does not authorize a
tool, grant permission or prove an external operation completed. If checkpoint storage
fails, the coding model is not called with an uncommitted reduced context. Ambiguous
storage errors are surfaced without a second attempted summary commit.

## Accounting and observation

Every summary call, retry and chunk contributes provider usage and run token charges,
including calls ending in failure or cancellation. Missing usage uses a conservative
input-plus-reserved-output estimate and marks token accounting as estimated. Each call
checks remaining budget and clamps output capacity before starting. Summarization shares
the run's duration and cancellation controls. No estimate is described as an exact tokenizer.

Automatic summaries contribute to the normal run progress and terminal statistics.
`ModelContextInfo` additionally reports optional `summary_attempts`, `summary_fallbacks`
and `last_summary_checkpoint`; resumed source metadata includes `summary_checkpoint`.
Summary work is counted as compaction, not an extra coding round/model request. Fallback
emits a compaction diagnostic followed by successful pruning. Manual operations return a
typed report rather than pretending to be a coding run's event stream.

`SessionBranch::accounting()` aggregates known durable usage. A terminal turn's statistics
own its automatic summary charges; checkpoints are counted separately only when their
owning turn has no terminal statistics. Manual checkpoints own their usage, and a failed
manual attempt writes `aicoder.context.summary_usage.v1` with usage, token accounting and
stop reason. Replaying a checkpoint incurs no new model cost. Unrecorded work lost in a
process crash, including a failed persistence write, cannot be reconstructed as known usage.

## Scope and verification

`cargo run --locked -p aicoder-runtime --example offline_summary` exercises real JSONL
storage, manual reduction, branch isolation, resume without regeneration, raw-history
preservation and durable usage using a scripted provider. An optional path exports the
resumed run's validated event-v1 trace. `semantic_summary` integration tests additionally
cover automatic summaries, retries/chunks, budget preflight, unknown usage, cancellation,
duration limits, malformed checkpoints, failed storage, tool-unit boundaries, opaque-state
omission, failure caching and uncertainty-preserving fallback.

This increment completes the semantic-summary and retry/accounting roadmap items. S4
still includes tool-output artifacts, richer navigation/model switching and tokenizer policy.
Session snapshots, canonical transcript buffers and projection still scale with loaded
history: this is not JSONL paging or a total-history memory bound. Scripted checks verify
contracts and preservation of supplied constraints; they do not measure a real model's
summary quality or coding success rate. See [acceptance and measurements](s4-summary-acceptance.md).
