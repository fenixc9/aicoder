# Independent session fork and clone

Agent::fork_session prepares, guards and publishes a new session from an existing selected branch.
PreparedSessionFork / apply_fork let a native host inspect the same owned plan before adoption.
The source lease spans preparation, validation, guard work, publication and completion observation.
The target ID and records are fixed before the guard. This operation generates no model requests
or tool effects and does not rewind workspace files.

| ForkPoint | Copied prefix | Editor |
| --- | --- | --- |
| Current | Entire selected ancestry, with a new session ID | None |
| Root | Empty conversation | None |
| BeforeLastUser | Ancestry before the latest selected-branch User | Selected text draft |
| Before(ID/label) | Ancestry before that canonical User | Selected text draft |
| At(ID/label) | Ancestry including the selected entry | None |

Entry/label resolution is branch-aware and fixed before callbacks. Before requires User, not a
custom message projected as a provider User. Raw IDs and ancestry remain intact for copied records,
including summaries, model choices, reasoning epochs, custom/native context and tool result/media
references. Source run IDs and usage remain historical observations, not new billed requests.
Source inboxes, grants, callback registrations and backend/transport handles are never copied.

Global navigation metadata needs explicit normalization. A copied label record targeting an
uncopied branch is retained as inert aicoder.session.fork.omitted-navigation.v1 data; its original
payload/ID/parent remain traceable. Current resolved labels on copied entries and the global title
are appended to the new target. Summary IDs/direct-parent anchors remain valid. A partial turn at
the cut is closed by a fork-boundary record that retains uncertainty and unknown accounting. Raw
unpaired tool history stays raw; existing model projections handle incomplete units separately.
The source is never repaired or rewritten by forking.

SessionForkLimits defaults to 10,000 target records / 16 MiB complete output, scanning up to
100,000 source records / 128 MiB and retaining at most 64 KiB editor text. Trusted hosts may choose
validated bounded limits. Editor text clipping and omitted image blocks are separate metadata.
Restoring text is not an image fetch or a new user submission; users select images explicitly.
Frontends preserve a newer draft typed while the operation was waiting.

JSONL preparation opens and fingerprints the source, reads the complete admitted prefix through
archive locations and bounds records/indexes/serialization. Cold descriptors never become copied
records or prompts. The target's bounded window is materialized before publication; an unreadable
new window fails before a target exists. Large outputs are partitioned into admitted transaction
records while preserving frozen IDs and final revision. Memory storage follows the same domain plan.

After compatibility validation and SessionHook before-Fork approval, publication stages the entire
private file. It rechecks the opened source descriptor and named path, source bytes/revision/leaf,
cancellation and elapsed budget before a no-clobber hard-link publication. Existing targets are
never overwritten. The target lease is acquired before visibility and held through observations.
Source/target authority is retired only after publication. Denial or pre-publication failure
leaves no target; owned staging is removed. Post-visibility identity/durability failure returns
SessionForkPublishFailure with the visible identity and never republishes automatically. The source
binding remains current on that uncertain failure, and operators can inspect the new file by ID.

Successful publication emits Shutdown/Started with Fork reason and actual source/target positions.
Observation failure, deadline expiry or cancellation after publication retains the completed target
and reports incomplete observation. Backend pins remain opaque host objects; serialized provenance
under aicoder.session.fork.v1 contains only source IDs/revision/leaf, point, counts and ignored torn-tail
bytes. It cannot restore IO or authorization. Ordinary continuation validates the target again.

```sh
aicoder fork-session SESSION_ID
aicoder fork-session SESSION_ID USER_ENTRY_OR_LABEL
aicoder fork-session SESSION_ID ENTRY_OR_LABEL --at
aicoder clone-session SESSION_ID
cargo run --locked -p aicoder-runtime --example offline_session_fork -- --window --before
```

TUI /fork, /fork-at and /clone use the owned session job. The optional-argument palette inserts a
valid command rather than its help placeholder. Native private choices expire on cancellation;
paste remains data. A restored editor is a draft and requires a separate submission.

RPC fork_session {point} targets the host's selected session, shares its one owned mutation slot,
and returns session_id plus inert fork editor/provenance/lifecycle data. cancel_session_selection
targets the owned request ID. Web POST /api/session/fork accepts {id, point}; GET
/api/session/operation observes it, with scoped /api/session/operation/ID/ui and /cancel controls.
Authentication precedes bodies, principal/workspace/provider are fixed by the host, and source IDs
cannot select another workspace. Busy/dedup/admission limits apply across runs, commands and forks.
Browser disconnect only stops observation; shutdown cancels and awaits the operation owner.

The pinned Pi fork runtime consumes cancellation and before/at position. Its exported declaration
mentions skipConversationRestore, but the installed fork path does not consume it; that declaration
alone is not runtime behavior to reproduce. This API preserves the copied conversation explicitly.
Tree-navigation summaries, compaction interception/replacement and boundary context-edit/compaction
drafts remain open in the [audit](pi-parity-audit.md). See [acceptance](s7-session-fork-acceptance.md).
