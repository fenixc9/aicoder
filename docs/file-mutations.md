# Prepared edits and approval diffs

Runtime file mutation now follows one path: **prepare → review → commit**. Preparation
reads a bounded snapshot, validates exact replacements and freezes the new bytes. The
approval callback/RunHandle receives that plan's preview. Approving consumes the same
owned plan; it does not reopen the file and silently apply a newly calculated change.

## Tools and revisions

`edit_file` retains its existing exact-match rules: 1–32 replacements, every nonempty
`old_text` occurs exactly once in the original UTF-8 file, and original ranges cannot
overlap. All replacements validate before staging. Edit text totals at most 1 MiB;
original and resulting files are each capped at 4 MiB. Missing/ambiguous/overlapping
matches retain their typed errors. A final identical result returns `no_change`.

`write_file` creates or replaces UTF-8 content, with a 4 MiB content and original-file
limit. It can replace binary originals, but their preview is explicitly omitted.
Existing parents are required. Replacing identical content now returns `no_change`.
Both tools recheck size after a bounded read, rather than trusting an earlier stat.

Successful `read_file` pages and mutation receipts contain `revision`. Pass it as
optional `expected_revision` to either mutation tool when protecting a previous read:

```json
{"path":"src/lib.rs","expected_revision":"<revision from read_file>","edits":[{"old_text":"old value","new_text":"new value"}]}
```

For create-only writes use `"expected_revision": "missing"`. A mismatch returns
`edit_conflict` before asking for approval. Revisions identify length, modification
time and Unix device/inode using a versioned noncryptographic fingerprint. They are
ordinary-change guards, not content hashes or authorization tokens; a deliberate
same-inode rewrite that restores timestamps may evade this optional read guard.
Regardless of that optional argument, every prepared plan retains original bytes,
identity and permissions and checks them again at commit.

Receipts retain `path`, byte counts and edit counts, plus the new revision. The complete
receipt must fit `ToolContext.max_output_bytes()` before mutation is allowed; otherwise
`output_limit_too_small` leaves the file untouched. A custom host executing a prepared
plan with another context also has that output/workspace checked.

## Approval and extension API

Core adds an object-safe `ExecutableTool::prepare` method with a default `Ok(None)`.
Existing custom tools continue through `execute`. Returning `Some(Box<dyn PreparedTool>)`
opts into pre-approval preparation. The dispatcher runs preparation once, publishes its
preview, then consumes `PreparedTool::execute` after approval. A denial, preparation
error, oversized preview, cancellation or steering drops the plan without execution.
Custom preparation must have no mutation side effects; core cannot sandbox Rust code.

`ToolInvocation.preview` is optional `ToolPreview { summary, body, format, truncated }`.
The whole serialized preview is capped at 32 KiB by core. Format is an open string;
runtime supplies human-readable unified-diff display data. The runtime diff formatter
uses [similar](https://docs.rs/similar/3.2.0/similar/) with a 100 ms algorithm deadline,
256 KiB combined text and 8192 combined-line limits, and a 16 KiB body writer. Oversized
or binary sources get an explicit omission notice. Escaping/metadata also count toward
the 32 KiB cap. `truncated` means the preview is partial/omitted; the prepared replacement
still contains the complete operation. Preview text is for review, not a patch-apply API.

CLI approval displays the diff when present. TUI approval uses PgUp/PgDn for scrolling,
Tab to switch diff/arguments, y/n to decide and Esc to cancel. Decision controls stay
visible while scrolling. `display_preview` escapes terminal/control-direction characters
without changing the actual replacement bytes. Explicit automatic approval still exists;
project settings cannot enable it.

Both `ToolApprovalRequested` and controlled `RunApprovalRequested.invocation` expose the
same optional preview for Web/RPC/bench consumers. Event version remains 1: old records
omit the field and still deserialize. Rust struct literals/patterns need the new optional
field or `..`; see [migration](migrations.md). The plan, file descriptors and original
snapshot are never serialized into events/history. Source diffs are normal workspace
data, so treat exported traces with the same access policy as tool read results.

Preparation and execution have separate tool-timeout bounds; approval remains subject
to run cancellation/duration. Steer/cancel can interrupt pending preparation. SDK callers
using `prepare` directly own preparation timeout/approval policy; calling the bundled
`execute` directly wraps both preparation and commit in the context timeout.

## Publication and conflict boundaries

On the validated Unix path, runtime pins an open parent-directory descriptor and opens
files relative to it with no-follow/nonblocking flags. Canonical workspace/path checks
remain in place, including revalidation after review and staging. Dangling final links
are rejected. A normal symlink resolving inside the workspace may be used explicitly;
retargeting that alias or replacing the parent after preparation causes a conflict.
Recursive discovery still does not follow links.

Participating writers use stable advisory locks in a private 0700
`$TMPDIR/aicoder-file-locks-<effective uid>` directory (the platform temp directory when
TMPDIR is unset). Its 0600 regular lock files reject links/foreign ownership. There are
at most 256 hash stripes, bounding retained coordination files; unrelated paths can
occasionally serialize. Do not unlink this lock store while writers are active. No lock
files appear in the workspace or generated Git patch. Read snapshots and diff computation
run on blocking workers; plan snapshots share immutable bytes across validation workers.

After taking a lock, commit verifies original bytes, identity and mode, then creates a
private same-directory staging file with `O_EXCL`. It writes, flushes and synchronizes
that file, checks the snapshot again, and performs a final path/identity check without
an await before publication. New files use an atomic no-clobber link operation; an
external creator cannot be overwritten. Existing files use same-directory atomic rename.
The parent directory is synchronized before success is returned.

Locks coordinate these tools, not arbitrary editors. Full-byte checks detect edits during
review/staging, including same-length changes and metadata-preserving content changes.
There is still a check-to-rename interval for a nonparticipating external writer: this is
not a kernel compare-and-swap or transaction across arbitrary processes. Descriptor
pinning limits path redirection, but hostile concurrent ancestor moves are outside the
filesystem/tool sandbox guarantees. Inspect/re-read after `edit_conflict` before retrying;
never silently rebase an approved plan onto a changed file.

New files are private 0600. Replacement preserves the original lower nine permission bits
(including executable bits), while clearing set-ID/special bits. Atomic replacement
creates a new inode: ownership, ACLs, xattrs and hard-link relationships are not preserved.
This contract has been validated on Linux; it does not establish Windows support or
portable network-filesystem durability.

## Cancellation, errors and durability

Staging paths have an RAII cleanup guard. Cancellation/timeout before publication drops
the guard; pending Tokio writes may finish only on the unlinked private inode. They
cannot later rename/publish a file. Preparation/validation workers perform reads only.
There is no detached commit job after the tool future is dropped. Abrupt process death,
permission changes or cleanup IO failures can leave a private `.aicoder-*.tmp`; cleanup
is best effort under those conditions.

Publication and the final directory sync are synchronous operations. They are not forcibly
preemptible filesystem calls. Cancellation can coincide with an already completed external
change before the session checkpoint is recorded; the existing interrupted-tool recovery
contract still requires workspace inspection. There is no transaction spanning the file
and JSONL history.

If directory synchronization fails after publication, `durability_uncertain` includes
`committed: true` and the receipt. The change was published but crash durability is unknown;
inspect it before retrying. Earlier staging/write/sync errors leave the original unchanged
when publication has not occurred. File errors/limits remain structured tool results.

```sh
cargo run --locked -p aicoder-runtime --example offline_edits
```

The example exercises real RunHandle diff approval, successful publication and preserved
external conflicts, with a scripted provider. An optional path argument exports its full
trace. See [acceptance evidence and cost review](s3-edits-acceptance.md).
