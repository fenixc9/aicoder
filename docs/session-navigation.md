# Session navigation and model selection (S4)

Core now exposes idle session operations through `SessionNavigator`, paged tree metadata,
literal visible-text search and model compatibility checks. CLI/TUI use these APIs over
the same repository. No new provider, frontend type or dependency is introduced into core.

## Branches and metadata

`SessionNavigator::new(repository, workspace)` pins the canonical host workspace. `open`
rejects foreign-workspace IDs. Rename, label, checkout, branch and recovery reopen a fresh
snapshot and acquire the repository run lease; an active run or conflicting revision fails.
No intermediate checkout is persisted when creating a branch: its marker and new selected
leaf commit atomically. Other branches, entries, summaries and artifact references remain.
Returned snapshots do not retain run ownership tokens.

New metadata uses reserved extension `aicoder.session.navigation.v1` with tagged operations:

- `rename {title}` changes the session title globally across the tree.
- `label {entry_id, label}` sets/removes a navigation label on an existing entry; null removes it.
- `branch {name}` creates a new branch marker at its recorded parent; name is optional.

Titles and labels are nonempty single-line names of at most 256 UTF-8 bytes, with control
and direction-override characters rejected. At most 256 labels can be live. Batch validation
includes staged labels, so an invalid/over-quota batch cannot partly rename or relabel a
session. Labels and renames fold over the full append-only tree, independently of checkout.
They never become model messages/instructions. The automatic initial title remains until
explicitly renamed. There is no hidden clearing/reset operation.

`Session::labels()` returns an ordered borrowed map from a bounded derived index. The
index is rebuilt from committed metadata on replay, not an authority restored from history. `resolve_entry` accepts an exact ID
or unique label. Exact IDs win, duplicate labels are an explicit ambiguity, and unknown
references are rejected. A label may point to a message, model change, terminal record or
branch marker; it does not have to name a current tip. Branching from the empty root is
supported without inventing a user message.

`recover` validates the selected projection and closes a vanished open turn with a recovered
terminal record. It is idempotent when no open turn exists. It neither retries commands nor
records synthetic missing results as historical facts. Checkout/fork may select an earlier
partial turn; safe projections retain the existing interruption/uncertainty rules.

## Bounded read views

`tree_page(limit, cursor)` returns 1–200 rows per page: entry/parent IDs, depth, kind,
selected/selected-ancestry flags, tip status, label and a short visible preview. It does not
dump raw tool results, reasoning or opaque provider state. The existing CLI `session-tree
--json` remains the explicit full raw-tree inspection path for compatibility.

`search_messages(query, limit, cursor)` searches literal, case-sensitive text **within stored
Text blocks**, across all branches. One hit is returned per matching message, with entry ID,
role and a bounded surrounding snippet. It does not concatenate a phrase across separate
blocks, search tool JSON/opaque state/reasoning, or provide regex/fuzzy search. Query is
1–256 UTF-8 bytes; each page returns at most 100 hits and scans at most 512 entries,
4096 blocks and 256 KiB of text, including overlap. Unicode boundaries and overlap retain
matches crossing byte-page boundaries. A page can have no hits and still have a continuation;
clients must follow `next` before concluding that the remaining history has no match.

Cursors bind session ID, snapshot revision and (for search) query. Invalid positions,
foreign sessions, changed queries and stale revisions fail explicitly. Tree rows remain
chronological; selection changes invalidate cursors even if messages did not change.

These are bounded **returned views/scans over an already loaded snapshot**. Tree setup
still traverses the tree/selected ancestry, and JSONL/session snapshots still load complete
archives. This increment does not bound total archive residence, introduce a disk index,
or make the old full-tree export a paged API. Storage paging/loading budgets remain open
before accepting S4's overall long-history memory requirement.

## Model choice and preflight

`Agent::switch_session_model(repository, id, model, context)` validates before persisting
`ModelChanged`. It checks workspace/run ownership, requires explicit recovery of an open
turn, freezes host instructions, checks the model/adapter and projected history, then commits
only the model identity. It generates no summary, model response, tool execution or fake
coding turn. Re-selecting the same identity is a no-op. Errors/cancellation before commit
leave an existing session unchanged. A late cancellation cannot roll back an atomic commit
already in progress.

`TurnExecutor::prepare_input` freezes an input-context provider once; prepared input retains
private provenance and start time. `validate_model_selection` inspects that projected view,
configured tools/options and `ModelSpec`. Typical checks cover images, complete tool pairs,
tool support, provider-opaque replay, sampling/structured options and output limits.
Reasoning-only history may be omitted from the model view for models that do not support
it; canonical history is retained. Common validation borrows the request and clones only
when reasoning adaptation or the empty-branch compatibility probe is needed.

`ModelProvider::validate_request` is a new default **pure local** hook. Adapters can reject
replay formats/options their wire protocol cannot encode. Chat-completions checks its
image roles/sources, opaque state and DeepSeek-specific option restrictions here; those
failures no longer wait until a model selection/new turn was written. The executor and
actual provider generation also validate before sending a request.

A known model context window is checked against estimated input and output reservation.
A configured compactor must target a window compatible with the selected model; otherwise
hosts must rebuild their policy. Oversized input is allowed only if the configured policy
actually triggers compaction, and the report says `needs_compaction`. No compaction is
executed by preflight, so later semantic compaction can still fail or exhaust budget. When
model window/output metadata is absent, the report preserves that uncertainty (zero output
reservation means no known reservation, not zero generated tokens); estimates
are not an exact tokenizer or proof of remote availability.

Normal session turns now freeze/validate input **before** closing a prior turn or appending
model/turn/user records. A prepared input is reused for execution rather than preparing
resources twice; provenance is still journaled before the first request. Preparation obeys
cancellation and the duration budget, and elapsed preparation time is included in the same
run budget. SDK callers preparing an input for later execution should account for that
anchored duration. Standalone run APIs retain their existing preparation path.

## Provider routing and continuation

SDK `Agent::new` keeps configured-model behavior. Opt into recorded branch models with
`following_session_models()`. It follows the chosen branch's model only when its recorded
provider matches the host provider; history never selects an endpoint, credential reference
or provider profile. Switching providers requires an explicitly constructed host/Agent for
that trusted provider. Selecting a different branch restores its model identity.

The strict `ChatClient` registry behavior remains. `ChatClient::for_model` explicitly selects
another name on the exact existing service, retaining registered metadata, the HTTP pool,
proxy configuration and request-time credential provider. It does not read credentials.
`Runtime::session_client()` wraps that operation for CLI/TUI continuation and selection.
Registered specs win; otherwise the **declared protocol profile** applies. Capabilities are
not guessed from names, no remote model discovery is performed, and server availability is
not tested. Hosts needing a closed catalog or model-specific limits can use the strict client,
register specs or inject another provider. The metadata/report contains no private route.

CLI continuation follows recorded models unless `--model` or `--profile` explicitly selects
host configuration. Environment/file model defaults still apply to new sessions; use an
explicit flag to replace an existing session choice. TUI follows branch models and applies
explicit startup model/profile selection when `--session` is supplied. Manual summaries
retain their separately configured host summary model; a branch choice does not rewrite
that policy or any saved configuration.

## CLI and TUI

All metadata commands work without model generation; selection uses local metadata and
needs no resolved bearer credential:

```sh
aicoder rename-session SESSION_ID 'project notes'
aicoder label-session SESSION_ID ENTRY_ID stable
aicoder label-session SESSION_ID ENTRY_ID --remove
aicoder branch-session SESSION_ID --from stable --label experiment
aicoder branch-session SESSION_ID --root
aicoder checkout-session SESSION_ID stable
aicoder recover-session SESSION_ID
aicoder session-branches SESSION_ID --limit 50
aicoder search-session SESSION_ID 'literal phrase' --limit 20
aicoder switch-session-model SESSION_ID MODEL_ID
```

Read pages accept `--cursor JSON` returned by the previous page. IDs are scoped to
`--workspace`; tree inspection, checkout, migration and deletion now consistently reject
foreign workspaces. Root checkout keeps `--root`.

TUI adds `/tree [CURSOR_JSON]`, `/search QUERY`, `/search-next CURSOR_JSON`, `/name TITLE`,
`/label NAME`, `/unlabel ENTRY_OR_LABEL`, `/checkout ENTRY_OR_LABEL_OR_ROOT`,
`/branch [ENTRY_OR_LABEL_OR_ROOT]`, `/recover` and `/model MODEL_ID`. Arguments are the
raw remainder, so titles/labels can contain spaces without shell quoting. `root` is a
reserved TUI target; IDs always disambiguate. Label targets the selected entry; branch
without arguments forks the selected leaf, then `/label` can name it. Commands execute
while idle; model checks run asynchronously with cancellation. Missing required arguments
remain command errors and cannot be expanded into a template/model prompt.

The header reflects the branch/actual run model. Successful navigation/model changes retire
old TUI session-grant handles; read-only views do not. Failure preserves the previous model
header and grants. Metadata actions add no user chat message. This retirement is frontend
host policy: core navigation metadata never recreates authority, and custom hosts own their
own ephemeral grant lifecycle.

JSONL stays v3 and event envelopes v1. New extension records remain inspectable by older
binaries, which ignore their navigation semantics; downgrade is not equivalent metadata
replay. See [migration notes](migrations.md) and [acceptance](s4-navigation-acceptance.md).
