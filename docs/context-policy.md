# Token counting and context policy (S4)

Core now shares token counting across model preflight, context decisions, built-in pruning,
semantic summaries, request budgets and unreported-response accounting. Hosts can inject a
counter and `ContextPolicy` without replacing the agent loop. Runtime offers optional local
BPE encodings; default builds retain streaming heuristic counting and no tokenizer
runtime dependency.

## Precision and scope

`TokenCount` records a token number, scope (`request`, `output`, `text`), accuracy
(`estimated`, `exact`), method and optional fallback reason. Exactness is scoped to the
thing counted; it is not an assertion about a provider's billing or hidden work.

| Counter | Text | Whole request/output |
| --- | --- | --- |
| `HeuristicTokenCounter` | Estimated | Estimated |
| `TokenizedCounter` + exact `TextTokenizer` | Exact for the declared encoding | Estimated |
| Host `TokenCounter` | Host-declared | Can be exact when the host knows the full model/protocol format |

The built-in BPE request calculation counts provider-neutral serialized messages, tools,
response format/tool choice and fixed framing allowances. It does not certify the server's
chat template, hidden reasoning, vision accounting or injected tool instructions. Images
and opaque provider state fall back to heuristic request counting. Encodings are explicit;
model-name prefixes never silently choose a tokenizer. A hint is read only from a supplied
`ModelSpec.tokenizer`.

Provider usage remains separate and authoritative when consistent. Complete reported
input/output or total usage owns run charges. Missing usage uses the counter's input plus
counted/retained output or a reserved completion bound, marked estimated. Partial reported
fields provide a floor; a contradictory total below reported input/output is not used to
undercharge and is marked estimated. An exact text/input count never turns unreported
billable usage into a false provider-reported total.

## Counter API and fallback

`TokenCounter::count(TokenInput)` receives provider/model identity, optional tokenizer hint
and borrowed request/output/text content. Request content includes tools, response format
and tool choice. `TokenCounting` validates results and centralizes explicit fallback to
`HeuristicTokenCounter`; invalid methods, wrong scopes, zero counts for nonempty content,
contradictory precision or typed counting errors cannot silently bypass a budget.

Fallback reasons are bounded enum values: missing/unavailable/unsupported tokenizer,
unsupported content, input limit, counter failure or invalid count. No private backend
error body or input text is added to diagnostics. The default heuristic is intentionally
selected, estimated, and has no fallback reason. Automatic mode reports why it fell back.

```rust,ignore
let executor = TurnExecutor::builder(provider)
    .workspace(workspace)
    .token_counter(std::sync::Arc::new(my_counter))
    .context_policy(std::sync::Arc::new(my_policy))
    .build()?;
```

Counters and policies are trusted host implementations: their functions must be pure,
bounded and make no model/tool/network requests. A host counter claiming exact full-input
counts owns that model/protocol calibration. `TextTokenizer` is a separate exact text
encoding contract; unsupported encodings should return a typed fallback reason.

The heuristic streams JSON into a UTF-8-aware counting writer rather than allocating
another entire serialized message. Legacy `estimate_context_tokens` delegates to that same
heuristic and retains existing default counts. Tokenized counting limits serialized/text
work to 2 MiB per count; it does not silently split a BPE string into independently tokenized
pieces and call their sum exact. Oversized data falls back visibly. Ordinary text is encoded
literally, including strings resembling special-token spellings.

## Optional local BPE

Runtime's `tokenizer-tiktoken` Cargo feature enables `tiktoken-rs` **0.12.0** with no default
features. Only the local `cl100k_base` and `o200k_base` choices are exposed. Vocabulary
assets are supplied by the pinned dependency; no runtime vocabulary download is performed.
The adapter calls [`CoreBPE::count_ordinary`](https://docs.rs/tiktoken-rs/0.12.0/tiktoken_rs/struct.CoreBPE.html#method.count_ordinary),
which returns the ordinary encoding's token count without returning a token vector.
See the [library source](https://github.com/zurawiki/tiktoken-rs) for encoding definitions.

Settings use normal explicit > environment > project > user > default precedence:

```json
{"version":1,"settings":{"context":{"tokenizer":"auto"}}}
```

`AICODER_TOKENIZER` and `context.tokenizer` accept `heuristic` (default), `auto`,
`cl100k_base` and `o200k_base`. Auto uses only an explicit model-spec hint. Missing hints,
unsupported hints and feature-disabled tokenizers produce estimated counts with reasons.
Explicit BPE settings without the Cargo feature are configuration errors. No prompt/model
API request or credential resolution is required to count text.

```sh
cargo run --locked -p aicoder-runtime --example offline_context_policy
cargo run --locked -p aicoder-runtime --features tokenizer-tiktoken --example offline_context_policy -- cl100k_base
# Application builds may enable the dependency feature explicitly:
cargo run --locked -p aicoder-cli --features aicoder-runtime/tokenizer-tiktoken -- --prompt 'inspect this project'
```

A fixed encoding overrides a hint only because the host explicitly selected it. Applying an
OpenAI-family encoding to another provider remains a request estimate; it does not establish
that provider's tokenizer. Hosts can inject other local/remote-format-aware implementations
without adding them to core or default runtime builds.

## ContextPolicy and compaction

`ContextPolicyInput` exposes the model spec, borrowed model view/tools/options, count,
completion reservation, configured compactor target and any legacy compactor estimate.
A policy returns `Keep`, `Compact {target_tokens}` or `Reject {code}`. It decides when/where
to compact; `ContextCompactor` implements how. Custom policy logic can reserve headroom,
choose model-specific thresholds or reject a benchmark case without rewriting history.

The default `WindowContextPolicy` retains configured-window behavior and checks known
model capacity. A custom policy cannot keep a request above a known model input budget,
request a zero/oversized target, or request compaction without an installed compactor.
Rejections are explicit, and their rounds close before terminal events. Session preflight
uses the same decision before existing-session writes; manual summaries also respect the
policy target/rejection while still explicitly forcing a summary when the policy says Keep.

Built-ins receive `ContextMeasure` and the chosen target through `compact_counted`.
Pruning retains system messages, newest complete units and configured recent context.
Additive counters select a candidate in one pass; nonadditive counters remove older units
geometrically and recount complete candidates instead of subtracting BPE guesses. This may
remove more old units, but never splits tool-call/results or violates retention to force a
fit. Recounted targets and reduction are checked, as is the declared runtime system prefix.
A failed compactor retains consumed usage and leaves canonical progress untouched.

Semantic compaction uses the same counter with the **summary model's** spec/hint, not the
coding model's. Input checks, completion clamping, retries and fallback charges use those
counts. Coarse chunk selection is verified against actual counted requests; nonadditive
counts use bounded candidate shrinking. Summary failures/fallback pruning preserve their
accounting. Failed-source cache keys include target/count provenance, so changing a counted
policy does not blindly reuse a failure from a different target.

Legacy `ContextCompactor` implementations remain source-compatible through default methods.
Their old estimate still participates in triggering, while the counted wrapper rechecks
returned views against the active counter/target. To honor a dynamically selected target
inside a custom strategy, implement `compact_counted`; its scope/prefix/tool/history
invariants still apply. Conservative failure is preferable to pretending the old strategy
honored an incompatible target.

## Reporting, compatibility and limits

`ModelContextInfo` adds optional last actual input count, last context-decision count,
policy identity and decision. The old `last_input_token_estimate` is the same numeric input
count for compatibility; consult provenance for accuracy. `ModelSelectionReport` carries
optional count/decision as well. These are latest-state summaries, not a complete per-call
billing ledger. CLI diagnostics and TUI model-selection feedback show accuracy/source.

Event schema v1 gains optional definitions, old JSON reads with None fields, and JSONL
remains v3. Rust source literals of `ModelContextInfo` need the added optional fields.
The updated main published SDK trace records heuristic provenance; legacy frames remain
valid. Default and feature-enabled examples validate complete traces, including compacted
provider views and preserved canonical history.

This increment does not implement history paging or a total snapshot-memory bound.
Counters operate on an existing projection; synchronous host code/serialization cannot be
preempted like async model IO. Exact tokenization/calibration and remote billing are distinct.
The [acceptance record](s4-policy-acceptance.md) separates default build cost, optional BPE
cost, contract fixtures and real-model limitations.
