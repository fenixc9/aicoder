# Bounded archive windows and canonical paging (S4)

Runtime now streams JSONL validation and materializes a bounded run tail instead of
retaining every historical payload. Core exposes `RunSession`, `SessionWindow` and archive
page contracts; memory/custom repositories keep compatible full-snapshot defaults. CLI/TUI
and the Agent's turn, summary and model-selection paths use run views. No database or new
dependency is introduced.

## What is retained

The canonical JSONL remains append-only v3, including inactive branches, original messages,
labels, summaries and artifact references. A window's `entries` are only real materialized
records, with their original IDs/parents. `RunProgress.messages` contains that canonical tail
plus runtime prefix/current generation; it is no longer a promise to return the entire
archive for a large session. `source.recorded_messages` is the full selected-branch count;
`source.archived_messages` identifies canonical records omitted from RAM.

With a durable summary, its real text and complete retained canonical tail are loaded.
Older message payloads stay cold. Without a summary, the oldest complete units are omitted
with a fixed model-only archive notice. Interruptions remain explicit. An oversized newest
tool unit fails the budget instead of being split or silently removed. A summary whose
retained tail exceeds the budget also fails explicitly. Hosts may adjust trusted limits;
unknown omitted source is not fabricated as conversation facts.

New summaries record `draft.archived_raw_messages`, identifying how much raw source was
not materialized (an older summary may represent it). Counts/anchors still reference the
full canonical branch. This prevents treating the locally loaded slice as the entire
archive and preserves tracing across successive checkpoints.

## Streaming index and writes

A private metadata index retains IDs, parents, tool identities, turn/model/summary/navigation
validation facts and offsets. It strips user/tool/opaque payloads and large instruction
provenance. Its message descriptors are internal validation data: they are never returned
as canonical records, sent as prompts or persisted. Requested pages and run tails read real
payloads from their original transaction offsets. Full historical tool boundaries are
validated before a provider view is used. Global names/labels/accounting and branch model
identity continue to use actual metadata.

The repository keeps at most one bounded index cache. It is an IO hint, not authority:
every reuse checks the complete file fingerprint. Backend cursors are immutable, opaque
snapshots; old clones still conflict. Appending verifies the whole archive by streaming
fixed buffers, retaining same-length edit detection, repository/run ownership, transaction
checksums and torn-final-batch repair. The archive is never copied into an `actual` Vec
merely to verify a write. Record serialization and migration retain their own budgets.

Metadata scans are linear in archive bytes. A cold open/rebuild scans the archive, and
fingerprint checks still scan bytes on reuse/write; this trades IO for fixed payload memory.
The index is bounded and can reject extremely many entries even when payloads are tiny.
It is not a persistent database index, constant-time reopen or a hard allocator RSS cap.

## Host budgets

`JsonlSessionRepository::with_limits(root, SessionLoadLimits)` configures trusted policy.
Project files/history cannot raise it. `limits()` exposes the frozen policy for bench hosts.

| Limit | Default |
| --- | ---: |
| Explicit full snapshot bytes | 16 MiB |
| One transaction/header record | 8 MiB |
| Materialized canonical message/summary bytes | 2 MiB |
| Materialized messages | 2048 |
| Indexed entries | 100000 |
| Logical metadata-index charge | 32 MiB |
| Archive bytes scanned per pass | 4 GiB |
| Whole serialized page | 8 MiB + 64 KiB |
| Session files enumerated | 10000 |

Run opening uses full snapshots only below both the snapshot and window byte thresholds;
larger archives use windows. Explicit `open()` keeps the full API and rejects an oversized
snapshot before an unbounded allocation. Index charging includes serialized descriptors,
owned-data allowance and fixed per-entry cost; transient parsing/projection and allocator
overhead are additional bounded costs. A record reader holds at most one bounded transaction.
The default limits protect loading, not arbitrary caller-owned Full Session or memory-store
objects; hosts can still retain large objects they explicitly construct.

`SessionLoadLimit` identifies the exhausted resource, requested amount and limit. Giant
records/newest units cannot be paged as malformed partial JSON. Large legacy v1/v2 files
require explicit migration before window loading; small legacy migration retains exact
backups. Existing one-record migrations beyond the record budget fail visibly rather than
bypassing the cap. Raw data is not discarded to repair such input.

## Read and mutation APIs

`SessionRepository::open_run` returns Full or Window. The default trait implementation calls
open for custom backends; window support adds explicit append/lease/navigation methods.
Window commits update real canonical tails and invalidate their old projected view; reload
before asking for a fresh model context. Run journal positions remain local to materialized
canonical input, while summary source counts/entry IDs remain global.

`archive_page(id, limit, cursor)` returns complete original entries (1–100 per page), subject
to the whole page byte limit. Cursors bind ID/revision/offset. Tree/search also have backend
paged entry points; the shared core search scanner reads bounded visible text from actual
payloads. Empty search pages can still have a continuation. Stale or foreign cursors fail.

`SessionNavigator::execute(ArchiveNavigation)` supports both representations for rename,
labels, root/current/explicit-parent branching, checkout and recovery. It holds the same
idle run lease; branch selection is preflighted before committing. Old Full-returning
navigator helpers remain for compatible snapshot callers. `info`, `tree`, `search` and
`archive` apply workspace scope without requesting a full payload snapshot.

CLI adds `session-archive ID --limit 20 --cursor JSON`. Bounded `session-branches` and
`search-session`, metadata mutations, continuation, summaries and model checks use these
interfaces. The legacy full `session-tree --json` inspection remains subject to snapshot
limits. TUI loads the real tail, displays its omitted-message count and retains branch model
and known usage; tree/search/navigation use backend pages. Original records remain available
through the CLI/SDK archive APIs.

## Compatibility and acceptance

JSONL stays v3 and event envelopes v1. SessionContextSource gains an optional omitted count;
old JSON defaults to zero. SummaryDraft gains an optional archived-raw count, also zero by
default. Rust literals of these structures and CompactionRequest need the new counts.
RunSession is explicit, and backend state/run tokens are never serialized with window data.
No provider URL/credential route comes from archive metadata.

```sh
cargo run --locked -p aicoder-runtime --example offline_archive
# Generate outside a measured reader process:
target/release/examples/offline_archive --generate /tmp/archive-case 4096
target/release/examples/offline_archive /tmp/archive-case
```

The [acceptance record](s4-archive-acceptance.md) separates normal smoke cost from larger
archive reader samples. These are scripted contracts and IO/RAM measurements, not real
model success-rate results. Loading budgets, not unlimited history retention in every SDK
object, establish S4's production continuation memory boundary.
