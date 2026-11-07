# Summary projection lineage

Manual and automatic semantic summaries can retain edited historical messages, omitted raw tails,
custom messages, sourced native context and reordered/split tool results. The retained boundary is
resolved through stable canonical record IDs. Raw records and context-edit records remain intact;
a reduced model view becomes usable only after its branch checkpoint is committed.

The ordinary Agent APIs require no new frontend loop:

```sh
cargo run --locked -p aicoder-runtime --example offline_context_edits -- --compact
cargo run --locked -p aicoder-runtime --example offline_context_edits -- --window --compact
```

Each variant edits the newest historical message, commits a semantic summary without needing a
new raw suffix, then continues through the existing Agent loop. It uses one scripted summary
request and one scripted main request, retains ten canonical messages and verifies the original
target. No credentials or external model calls are involved.

`SessionContext::into_owned` produces `OwnedSessionContext`: owned messages, optional canonical IDs
for each contribution and global source metadata. Summary/repair/uncertainty contributions have no
canonical message ID. `RunSession::project_context` handles full branches; window repositories may
implement `SessionRepository::project_window_context`. JSONL validates the immutable backend cursor,
revision and leaf, then overlays only real records already admitted to the run over its private
validation index. It does not load cold payloads, enlarge the load budget or expose descriptors.
An explicit active turn suppresses only that turn's pending interruption marker after pairing.
Normal window materialization borrows origins rather than allocating owned IDs for every message.

Before semantic compaction, `SessionJournal::compaction_projection` maps these IDs to the existing
canonical transcript positions, using its initial IDs and acknowledged checkpoint positions.
Reordered results keep their own IDs, and a raw result record contributing multiple projected
results retains one canonical position. Manual summary uses the same mapping over its pinned
session. Equality verifies the complete current model view; it never selects an ID by equal text.
Unknown IDs or a different projected view do not acquire lineage.

`CompactionProjection` is immutable native data, with fingerprints of the view and canonical
history, a declared runtime prefix and retained raw positions. It contains no repository handle,
model route or authorization and is never restored from JSON. Shape, prefix and content changes
invalidate it before summary IO, with zero-attempt accounting. `ContextCompactor::compact_projected`
is an additive optional method; existing implementations retain their counted method by default.
Custom window backends and journals may return no lineage and retain the old exact suffix path.
They must implement the read-only projection contract to summarize edited views through anchors.
Deterministic pruning does not request this extra journal projection.

The semantic strategy filters synthetic System markers while carrying the corresponding positions
through normalization. It selects complete units, finds a canonical boundary and retains the raw
suffix count from that position, including omitted records after the final visible message.
Source interruption/recovery uncertainty remains explicit even when an edit omitted its tool unit.
Requests remain bounded and tool-free. Failed/cancelled attempts retain their usage; no checkpoint
failure can repeat completed model work. Snapshot preparation and validation consume the same
elapsed budget; manual preparation rechecks the deadline before summary IO.

New `SummaryCheckpoint` records use **version 2** in the existing summary namespace. Counts include
all canonical message contributors: ordinary messages, custom messages and sourced native context.
A custom/native message may start the retained tail; Tool results cannot split a call unit.
Version 1 records keep their original ordinary-Message-only counting and boundary rules. They are
validated and replayed without rewriting. Older binaries that only understand v1 checkpoints must
be upgraded before continuing sessions containing v2; the top-level session version stays unchanged.
See [migrations](migrations.md) and [acceptance](s7-summary-projection-acceptance.md).

This closes projection-aware summary preparation for the built-in memory/JSONL Agent paths.
[Boundary context-edit/compaction drafts](boundary-drafts.md) now share owned preparation and
atomic publication. Compaction interception/replacement, tree-summary hooks and final isolated
Spark/Pi/SWE quality/release acceptance remain separate work.
