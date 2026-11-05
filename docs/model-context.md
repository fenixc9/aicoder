# Canonical history and branch model context

Execution now has two inputs: canonical recorded history for results/journaling, and a
separate provider-facing message view. Recovery repair, pruning and capability adaptation
can change the provider view without turning synthetic output into a historical fact.
Explicit [native boundary context](boundary-hooks.md) is a recorded message with source provenance;
it remains in canonical history and summaries. Inert boundary Data receipts remain metadata.
[Startup hooks](startup-hooks.md) may replace only the declared runtime prefix for a run; private
prefix text and active-tool selections are not restored from session metadata.
This completes S4's history/context separation item. Semantic summaries, output artifacts,
full session navigation and tokenizer policies remain separate S4 work.

## Branch queries without checkout

`Session::branch(leaf_id)` returns a borrowed `SessionBranch`; `None` selects the empty
root. `selected_branch()` selects metadata's current leaf. Unknown leaves fail. Constructing
a branch/context does not commit a transaction, change selected leaf/revision, or clone
inactive branches. It walks only the already loaded snapshot's selected ancestry.

```rust,ignore
let branch = session.branch(Some(alternate_entry_id))?;
let recorded = branch.chat_messages();
let context = branch.model_context()?;
let branch_model = branch.selected_model(); // metadata only, does not switch provider
```

`entries()` borrows the branch's message/metadata records; `messages()` iterates recorded
message views, and `chat_messages()` clones literal messages in recorded arrival order.
The session's complete `entries()` tree retains other branches. `model_context()` owns safe
model messages and borrows a parallel `origins` map from the snapshot:

| Origin | Meaning |
| --- | --- |
| `Recorded { entry_id }` | Data came from a recorded entry; grouped tool results may map one entry to multiple provider messages |
| `RecoveredToolResult { call_entry_id, call_id }` | No result was recorded; a fixed error result is temporary context only |
| `InterruptedTurn { entry_id, turn_id }` | Fixed recovery guidance from a stopped/open turn; open turns have no terminal entry ID |

Tool results are validated, grouped and reordered by assistant call order. Duplicate,
orphan, unknown or malformed results are rejected. Missing results in interrupted/legacy
history get the existing `execution_interrupted` result, telling the model to inspect
before retrying. Interrupted turns get the same fixed system guidance; partial response
text and arbitrary failure details are not promoted into instructions. Completed turns
cannot contain unresolved calls.

`context_for_active_turn(turn_id)` is explicit: the ID must match the branch's open turn,
and live calls must already be paired. It omits only that current turn's interruption
marker; historical interruptions remain. This avoids treating ongoing operations as failed.
Legacy calls before a new turn boundary are repaired separately. `Session::model_messages`
remains the compatibility accessor for the selected safe snapshot.

`SessionContextSource` records session ID, snapshot revision, leaf, recorded message count,
recovered/reordered result counts and interrupted-turn count. Revision is the loaded
session's global revision; a foreign branch's leaf can predate it. The borrowed map has
one origin per provider message; `into_messages()` drops that map when an owned provider
view must outlive the snapshot. Legacy Compaction and unrecognized extension records retain
their previous metadata semantics: legacy summary text is not automatically activated. The subsequent
[semantic-summary increment](semantic-summary.md) reserves a validated extension for new
branch-local summaries.

## Execution input

`ExecutionInput::from_request` preserves existing SDK request behavior. The ordinary
`run`, context/handler/controlled and journal entry points still work. New entry points
accept an explicit input: `run_input`, `run_input_outcome`, `run_input_controlled` and
`run_input_with_journal`.

For a read-only counterfactual branch run:

```rust,ignore
let branch = session.branch(Some(alternate_entry_id))?;
let request = ModelRequest::new(model, vec![
    Message::text(MessageRole::System, "trusted runtime instructions"),
    Message::text(MessageRole::User, "continue this branch"),
]);
let input = ExecutionInput::from_branch(request, &branch)?;
let result = executor.run_input(input).await?;
```

Request messages supply a leading runtime system prefix plus fresh input. The branch
supplies recorded history and safe projection; the existing model/options/tools fields
stay explicit. A bare executor does not persist that run or checkout the session.
Registered/approved tools can still have external effects; the branch query itself has none.
Context source IDs do not bind session authorization automatically; custom hosts still
inject their own trusted approval scope when one is needed.
Model/provider selection remains the host's choice and capabilities are validated normally.

A trusted host with its own context policy can use
`ExecutionInput::projected(request, history, system_prefix_len)`. Fields are private;
constructors validate that the declared complete leading runtime-system prefix in history
is also identical in the provider view. Derived historical system markers may follow that
prefix in the view. The API is host input, not an untrusted model rewrite endpoint.

An input-context provider sees canonical history. Replacing runtime instructions updates
that explicit prefix in both buffers and preserves model-only recovery guidance even when
it is adjacent to the prefix. Source provenance is still journaled before model work.
S3's project/skill/template preparation rules continue to apply.

## Session-aware execution and persistence

`Agent` validates the prior safe projection before appending a new turn. It then journals
recovery/model/turn/user records as before, and builds:

- Canonical input: runtime system instructions, literal recorded branch messages and the new user input.
- Model input: the same instructions and new input around the safe branch projection.

The source header is updated to the newly committed input snapshot; later resource metadata
or generated messages can advance the store revision. Synthetic result/interruption messages
remain exclusively in the provider buffer. Progress/output history is never seeded from the
repaired view. Generated real messages, steer/follow-up provenance and final outcomes still
checkpoint at canonical transcript positions, even when the two input lengths differ.

Pruning changes only the working model buffer. Reasoning adaptation also changes only the
outgoing provider request, preserving raw reasoning for audit. Token budgets use the actual
projected/adapted request estimate, not the size of unrelated canonical history. The executor
moves the initial view into its working buffer and moves the outgoing messages to the provider,
removing an extra initial-buffer clone and the previous per-request whole-message clone.
Canonical, working and wire-owned messages still exist; this is not a zero-copy provider API.

## Reporting and compatibility

`RunProgress.model_context` and optional terminal event-v1 `termination.model_context`
contain `ModelContextInfo`:

- `source`: optional verified session/branch snapshot metadata (none for host-defined projections).
- `initial_history_messages`, `initial_context_messages`: counts after initial instruction preparation.
- `requests`: logical `ModelRequestStarted` attempts; not HTTP retries or billing requests.
- `last_request_messages`, `last_input_token_estimate`: actual adapted request count and heuristic input estimate, absent before any attempt.
- `compactions`: successful compactions, not failures.

Reports store counts/IDs, not another copy of provider messages. Current input estimates
remain heuristic; provider usage accounting stays separate. The trace validator checks
request/compaction counts, presence of last-request metadata and bound session identity.
Older progress/terminal JSON without the field still reads as none. Event version is 1,
and JSONL version remains 3. Source literals of RunTermination/RunProgress need the new
optional field or constructor; see [migration](migrations.md).

`input_messages` and checkpoint positions now refer to canonical input. Recovery-era result
histories may contain fewer messages than older repaired results and preserve original tool
arrival order. Consumers needing safe provider context must use the branch projection or the
actual ModelProvider request; do not forward canonical `RunProgress.messages` blindly.

This API does not bound total history residence, page JSONL, persist pruning, activate legacy
summary records or introduce exact tokenizers. New semantic checkpoints are available through
the [semantic-summary contract](semantic-summary.md); other items remain S4 requirements. A branch view borrows an already loaded snapshot, and owned model/history
buffers still scale with their selected input. See [validation and scoped measurements](s4-projection-acceptance.md).

```sh
cargo run --locked -p aicoder-runtime --example offline_context
```
