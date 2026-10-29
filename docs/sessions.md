# Tree sessions and durable turns

`Agent` uses the same object-safe `SessionRepository` in SDK hosts and frontends.
`aicoder_core::session::MemorySessionRepository` and
`aicoder_runtime::session::JsonlSessionRepository` implement the same tree,
transaction, conflict and run-ownership contract. No database, service or new
runtime dependency is required. Custom repositories can reconstruct snapshots with
`Session::from_entries`, validate batches with `validate_transaction`, and fold a
persisted batch with `apply_transaction`. `SESSION_VERSION` identifies the current
schema. Store implementations use `run_token`/`attach_run_token` for ephemeral owner
capabilities; do not serialize them or expose them in shared history. Store/host code
is trusted to enforce these contracts. Custom repositories can attach opaque checkpoints
with `attach_backend_state`, inspect them with `backend_state::<T>()`, and clear them
with `clear_backend_state`. Treat checkpoints as immutable: cloned views retain the
previous attachment until replaced. `from_entries` restores neither checkpoints nor
run ownership. See [backend boundaries](runtime-backends.md).

## Entries and branches

Every immutable `SessionEntry` has an ID, optional earlier parent and timestamp.
`SessionEntryData` stores messages, model identities, compaction records, extension
JSON, and turn start/end records. Metadata stores the selected leaf and revision.
Appending chains new entries from that leaf; `checkout` selects an existing entry
or the empty root. It preserves other branches. IDs are unique within a session;
parent references cannot form cycles. A compaction preservation reference must
be an ancestor. Compaction/extension records are storage primitives; semantic
summaries and extension execution remain roadmap work.

```rust,ignore
let mut session = repository.open(id)?;
let old_leaf = session.metadata().leaf_id.clone();
repository.checkout(&mut session, Some(earlier_entry_id))?;
repository.append(&mut session, Message::text(MessageRole::User, "try another approach"))?;
// Re-select the original branch without deleting the new entries.
repository.checkout(&mut session, old_leaf.as_deref())?;
```

`entries()` borrows the complete tree. `active_entries()` traverses only the
selected ancestry; `messages()` provides borrowed message views and omits metadata.
`chat_messages()` clones raw messages for display/export. `model_messages()` builds
safe model context and should be used for continuation. Metadata such as summaries
is not silently inserted into prompts. A selected model record describes the branch;
the host still explicitly selects the provider/model for each `Agent` run. Capability
validation remains enforced by the executor.

## Durable execution and recovery

Before running, `Agent` acquires an owned run lease and atomically writes any prior
crash recovery record, a model change when needed, a turn ID, and user input.
Validated assistant tool calls are saved before dispatch. Each completed tool result
is saved before the dispatcher continues past its batch/barrier. Read-only calls may
finish in a different order; model context reorders their results by original call ID.
Rejected completion candidates and verifier feedback are saved before another model
request. Final messages, stop information, partial response, and accounting are saved
in one terminal transaction **before terminal state/run events**.

Cancellation, budget exhaustion and provider failures retain completed interactions.
Partial response fragments stay in `TurnRecord.partial_response`, separate from
valid messages/calls. TUI reload labels partial output and displays the recorded stop.
A process exit or dropped task may leave no terminal record. On the next run the
lease is available again, and `Agent` records recovery with unknown final statistics
(`statistics: None`), rather than inventing zero usage or a successful completion.

Missing results become **ephemeral error results** in `model_messages()`, explicitly
stating that an operation may have run partially and the workspace must be inspected
before retrying. They are not written into history as actual tool outcomes. Interrupted
turns also receive a fixed context marker; untrusted partial text or error details are
never promoted to a system instruction. Orphan/duplicate tool results and completed
turns with missing results are rejected. Recovery never re-executes a tool on its own.
There is no atomic transaction spanning arbitrary shell/filesystem effects and the
session log: a crash between the external effect and its checkpoint leaves an
uncertain outcome. Inspect the workspace in that case.

A persistence failure stops execution and retains in-memory progress. Finalization
failures turn an otherwise completed run into stage `Persistence`; an already
interrupted run retains its primary stop and adds `persistence_error`. A failed
checkpoint is latched: the session journal does not blindly retry an uncertain write.
The store may be read/reopened to inspect its actual state.

`TurnExecutor::run_with_journal` exposes optional storage-neutral `RunJournal`
checkpoints to custom hosts. Positions refer to the original transcript; tool
results reserve positions in call order even when only a subset finishes. `finish`
is awaited on every returned outcome, including cancellation, before terminal events.
Hosts must implement atomic/idempotent checkpoint handling and bounded storage I/O.
Task/process destruction cannot call `finish`. The default `run_outcome` uses a no-op
journal and performs no storage I/O. Session I/O is synchronous in the bundled stores
and is outside the execution time budget; asynchronous servers should account for it
when selecting their storage/hosting model.

## JSONL v3 transactions and ownership

The session header identifies version, ID, creation time and canonical workspace.
Every subsequent line is a complete transaction containing revision, time, all new
entries, selected leaf, and a 64-bit FNV-1a checksum over its serialized payload prefix.
This checksum detects accidental corruption; it is not authentication or protection
against a writer deliberately rewriting the file. A valid transaction must have
monotonic revision, valid ancestry/turn markers and a matching checksum envelope.

Normal commits append one transaction and synchronize file data. A syntactically
incomplete, unterminated final line is ignored on read and truncated on the next
successful commit. Its whole batch is discarded. Complete invalid records, invalid
schemas and checksum failures are errors. The session view changes only after a
successful write/sync. Length plus an entire-file fingerprint detects stale views,
including edits that preserve file length; these return `SessionConflict`. Current
validation reads the file on each commit, so very long histories still have linear
I/O cost; this implementation does not claim constant-time appends.

Separate metadata locks coordinate readers/transactions and stable run locks enforce
one running agent per session. Readers remain available during model/tool awaits.
Other runs, appends, branch selections, migration and deletion are rejected while a
run owns the lease. Owner writes retain a private nonce. Dropping the lease or exiting
the process releases ownership; different sessions can run independently. Lock files
are deliberately retained after deletion so waiting processes cannot lock a replacement
inode. These locks coordinate participating applications, not arbitrary editors.

On Unix the store uses private directories (0700), new files/backups (0600), and
rejects symlink/non-regular data and lock files. Validation has run on Linux; portable
filesystem durability and platform support follow the existing project baseline.
The log retains complete branches/results; pruning/archive policy remains future work.

## Legacy migration and CLI

Read-only open of v1/v2 does not modify the file. Explicit migration or the first
append/checkout creates an exact `<old-path>.v1.bak` / `.v2.bak`, synchronizes it, then
writes and atomically renames a v3 snapshot. Session ID, workspace, creation time,
entry IDs/timestamps and canonical content are retained; old entries become a parent
chain. A conflicting existing backup prevents migration. An interrupted migration
can be retried if the backup exactly matches the original. Backups are not listed as
sessions. `migrate` is a no-op for v3.

Stop older clients before upgrading a shared session directory: old writers do not
understand v3 transactions or the new run lease. To roll back, stop all writers and
restore the original from its backup; v3-only branches cannot be encoded by the old
format. Keep backups until satisfied with the migration.

```sh
aicoder sessions
aicoder session-tree SESSION_ID
aicoder session-tree SESSION_ID --json
aicoder checkout-session SESSION_ID ENTRY_ID
aicoder checkout-session SESSION_ID --root
aicoder migrate-session SESSION_ID
aicoder --session SESSION_ID --prompt 'continue from this branch'
```

`session-tree --json` emits metadata and all entries for inspection/export without
loading provider credentials. TUI opens the persisted selected branch; visual branch
navigation remains future frontend work. [RunHandle controls](run-control.md) provide steer/follow-up and scoped approvals.
[Versioned events and bounded subscription behavior](events.md) complete S2.
