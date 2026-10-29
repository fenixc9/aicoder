# Bounded tool output and recoverable artifacts (S4)

Tools now keep bounded previews in the canonical transcript and can retain original
output through an injected artifact store. The same references appear in tool-end events,
SDK results, CLI and TUI. Reading an artifact is a normal bounded ReadOnly tool call;
core adds no filesystem backend, provider dependency or implicit tool registration.

## Core API

`aicoder_core::artifacts` defines object-safe async `ArtifactStore` and `ArtifactWriter`,
`ArtifactRef`, raw-byte `ArtifactPage`, typed errors and limits, `store_bytes`, an in-memory
store and `ReadArtifactTool`. `TurnExecutorBuilder::artifact_store` or
`ToolDispatcher::with_artifact_store` installs the store explicitly. A host using only
`ToolContext` can call `with_artifact_store`. Core defaults install no store and no tools.
Hosts wanting model access register `ReadArtifactTool` in their chosen registry.

A writer accepts bounded chunks and publishes once through `finish(complete)`. Dropping
an unfinished writer discards its staged data/reservation. `complete` describes whether
the entire declared source was captured, independently of display truncation or paging.
References contain version, random opaque UUID, media type, byte count and completeness;
no local path or credential is part of a reference. References are data, not restored
approval grants. A host's scoped store determines access.

`ToolSuccess` and `ToolFailure` can attach up to eight validated references through their
`artifacts` vectors. Dispatcher output includes nonempty `artifacts` on the outer tool-result
envelope and in `ToolExecutionEnded`. Fixed reference metadata stays outside the payload
byte budget, like existing diagnostic fields, and remains recorded if a global output
budget cuts the preview. The run stops on that budget; it does not make another coding
request with an incomplete tool pair. Reading the original bytes is charged as a new tool
payload under the same normal output/run limits.

## Generic tool output

Unexpectedly large JSON tool output is serialized with a bounded writer. Complete JSON
larger than the display budget is saved as `application/json` when a store is available,
and the model receives a UTF-8-safe JSON-prefix preview, `truncated: true` and
`preview_format: "json_prefix"`. The preview is a string inside a complete ToolResult
block; it is not a malformed JSON record or a replacement for the assistant call.
Failure details have the same bounded preview/archive path and retain their failure code.

The serialization cap is the larger of the display sentinel budget and archive cap.
A small archive cap cannot shrink an otherwise valid small result. Inputs larger than
both budgets are bounded during serialization and report `artifact_error: "too_large"`
instead of claiming a complete archive. No store means deterministic clipping only.
If all eight reference slots are already occupied, a large JSON preview reports
`artifact_error: "reference_limit"` and preserves those references without creating an
unreachable additional file. Archive failure/quota/timeout does not re-execute a tool or change an already successful
operation into a failed operation. Errors are static codes, without private storage paths.
Archive publication attempts time out after 250 ms. Failure preview wrappers are included
in their detail budget; error code/message and fixed reference diagnostics have independent
bounds. An artifact-enabled dispatcher requires at least 512 display bytes.

## Shell streams

Runtime Bash retains its existing bounded prefix/tail previews, process-group cleanup,
combined scan limit and raw observed-byte accounting. When a stream first exceeds its
in-memory display capture, a writer is opened lazily, the retained complete prefix is
spooled, and subsequent raw chunks are appended. Staging uses disk streaming or the
host's writer, not an unbounded RAM copy of shell output. Small successful commands that
fit create no artifact files. Streams trimmed only by final JSON escaping can also be
archived from their retained bytes.

`ShellOutput` adds optional `stdout_artifact`, `stderr_artifact` and per-stream
`*_artifact_error`. Each stream has its own byte-exact `application/octet-stream` artifact;
there is no invented cross-stream ordering. The displayed text still uses `utf8_lossy`.
The SDK artifact reader returns original bytes, including NUL and invalid UTF-8.

A stream archive is complete only after EOF and while all its bytes fit its archive cap.
The cap preserves a partial prefix and sets `complete: false`; it does not stop the
command. A scan/time/capture stop can also leave partial artifacts, independently of exit
code. Storage errors disable archiving for that stream while normal bounded capture
continues. Begin/write/finish operations each have a 250 ms IO bound; command/tool/run
deadlines still apply. Dropping/cancelling the shell discards unpublished staging and
retains the existing process cleanup path. No partial file is published as a complete
artifact. A process disappearance may lose unrecorded bytes/references.

## Storage policy and retention

| Policy | Default | SDK range |
| --- | ---: | --- |
| Maximum bytes per artifact | 16 MiB | 1 byte–1 GiB |
| Store total admission budget | 512 MiB | at least per-artifact cap, at most 1 TiB |
| Stored + pending artifact count | 128 | 1–65536 |
| Raw bytes per read page | at most 64 KiB | request 1–65536 |

`MemoryArtifactStore` is scoped by its explicit instance, reserves the maximum size for
pending writers, and rejects quota exhaustion. It does not silently evict references.
Removing an artifact is an explicit host retention action through `ArtifactStore::remove`.
The byte quota is stored data/reservations, not a promise about allocator RSS or existing
caller-owned JSON values.

`aicoder_runtime::artifacts::FileArtifactStore` implements private storage. Runtime uses
`SESSION_DIR/artifacts/WORKSPACE_NAMESPACE`, derived from the trusted session root and
canonical tool/case workspace. Project settings cannot set that private root. The namespace
fingerprint is not an authorization check: the private footer also compares the full
canonical workspace on every read/remove. IDs must be canonical UUIDs. Regular files,
no-follow opens, a mode 0700 root directory/0600 files and bounded footer parsing protect the
model-facing path. This is workspace scoping, not per-session secrecy or an OS sandbox;
approved Bash still has the user's filesystem permissions.

Admission uses a short OS lock and accounts other-process reservations. A staged sparse
file reserves the per-artifact cap; its own OS lock distinguishes live writers from stale
process remnants. Complete records contain payload plus a bounded footer and are synced
before atomic rename under admission ownership. Directory scanning is bounded. Completed
file sizes include metadata when evaluating later admissions; simultaneously finalized
footers can add at most `max_artifacts * (16 KiB + 8)` above the reservation budget.
There is no unbounded archive index or background daemon. Limits apply per store/workspace
namespace; hosts own aggregate retention across workspaces.

`reap_incomplete(minimum_age)` explicitly removes old staging from vanished processes,
skipping OS-locked live writers. Dropped writers remove their staging. A crash, cancellation or journal failure
after publication can leave a final file without a recorded reference; it still consumes
the same quota. Hosts own final-file retention/removal and backups. Quota exhaustion is
visible and leaves existing files intact; there is no silent overwrite or eviction. The
format validates scope/identity/length, not a cryptographic proof against filesystem-owner
modification. Snapshots and JSONL still scale with loaded history; artifacts do not add
history paging or a total-history memory bound.

## Shared runtime and frontends

Runtime factories install the disk store and `read_artifact` when session storage is
enabled, including isolated bench workspaces. Bare `tools::default_registry()` stays an
explicit seven-tool registry without a store. `session_dir: null`, an explicit cleared
session directory, or CLI `--no-session` disables persistent artifacts. Construction/config
inspection creates no archive files; a large result creates storage lazily. Hosts can
replace the factory's store through the core builder without replacing the agent loop.

CLI/TUI display reference IDs, byte counts and completeness from tool-end events.
CLI reads an existing page without resolving model credentials:

```sh
aicoder --workspace ./project read-artifact ARTIFACT_UUID --offset 0 --max-bytes 8192
```

The SDK or a Web host uses `store.read(id, offset, limit)` for byte-exact downloads. The
model tool and CLI page display instead return complete bounded JSON with `content`,
`encoding: "utf8_lossy"`, raw `bytes_read`, `offset`, `next_offset`, `has_more` and the
actual stored reference. Byte offsets do not assert UTF-8 character boundaries. Hosts
needing exact binary exports must use raw `ArtifactPage.bytes`, not copy the lossy text.
A full page may still have more pages; source `complete: false` means unread source bytes
are unavailable even when `next_offset` is null. The page budget includes all metadata
and escaped text and never emits a broken cursor record.

Bench hosts can inject memory/disk/custom stores, record `store.limits()`, inspect typed
references in raw events/results and export bounded pages before deleting workspaces.
References alone do not make a trace self-contained: archives must accompany exported
runs when future byte-exact access is required. This increment does not implement the
remaining S7 grading/report-export work.

## Compatibility and verification

JSONL remains v3 and event envelopes v1. Missing artifact vectors deserialize as empty,
and empty vectors are omitted. Existing sessions remain readable. Rust source literals of
`ToolSuccess`, `ToolFailure` and `ToolExecutionEnded` require the new vectors; see
[migration notes](migrations.md). No provider protocol/content-block change is required.

```sh
cargo run --locked -p aicoder-runtime --example offline_artifacts
```

The offline example uses the real runtime registry, shell and JSONL store: a 200,013-byte
result is previewed, its hidden middle is read through the tool, and reopening retrieves
the exact original bytes. It validates a complete event trace and can export that trace
with an optional path argument. [Acceptance evidence](s4-artifacts-acceptance.md) also
covers quota, binary data, cancellation, foreign scopes, malformed files, serialization
bounds, global-budget checkpointing and the credential-free CLI read command.
