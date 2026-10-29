# Session tool authorization

Approval supports three explicit decisions: `Deny`, `AllowOnce`, `AllowSession`.
A session grant covers **one tool and capability, with all arguments**, in one host-owned
session/workspace and one registered tool implementation namespace. It is not a grant
for other tools, other conversations or all future application executions.

## Host ownership and lifetime

`ApprovalSessions` is a cloneable in-memory cache. Use one instance per trusted
principal/storage namespace; do not share it across unrelated repositories/users merely
because their session ID strings happen to match. Authenticating/routing a Web or RPC
client remains the host's responsibility. Session IDs, scope UUIDs and serialized metadata
are identifiers, not bearer credentials.

`TurnExecutionContext::with_approval_sessions(cache)` opts a session-aware `Agent` into
this cache. After selecting the repository session, Agent binds its canonical workspace
and ID before acquiring the run lease or appending turn history. An explicit scope
mismatching that session fails before those history writes.

```rust,ignore
use aicoder_core::{TurnExecutionContext, tools::ApprovalSessions};
let permissions = ApprovalSessions::default(); // retain in this host's session service
let context = TurnExecutionContext::new().with_approval_sessions(permissions.clone());
let outcome = agent.run_with_session_outcome(
    &repository, selection, prompt, observer, context,
).await;
```

A host with its own transcript/session model can obtain
`permissions.session(workspace, logical_session_id)?` and inject that `ApprovalSession`
with `with_approval_session`. A raw executor with only the cache has no selected session;
no session choice is available until the host supplies a scope. Read-only tools retain
their existing approval exemption. Workspace binding identifies the authorization
namespace; it does not restrict Bash filesystem/process access. Tool capabilities
are assigned by the trusted host.

Cloned caches/scopes share mutable authority; serialized `ApprovalSnapshot` and
`ApprovalSessionInfo` contain display data only. There is no restore-from-history or
restore-from-snapshot API. JSONL version stays 3 and no authorization is loaded from
project settings, AGENTS/skills, model messages, tool output or previous event traces.
A new process/cache starts empty even when reopening the same stored conversation.
CLI grants therefore last for the current invocation; TUI grants can span multiple
turns while that application is alive.

A registry has an opaque identity. Reusing a shared registry permits matching grants;
constructing another registry with the same tool name/capability does not inherit them.
New tool names/capabilities ask separately. Hosts changing a tool's semantics/policy
must use a new registry or revoke existing grants. Cache hits precede the interactive
decider; installing another callback alone is not revocation. Root identity includes
canonical path and Unix device/inode, so replacing an ordinary workspace directory
cannot transfer its old grants. Linux is the verified platform.

## Decisions and control API

`ApprovalHandler::decide(&ApprovalRequest)` receives the invocation, its prepared preview
and optional session information. Its default adapts legacy `approve(&ToolInvocation)`:
true means `AllowOnce`, false means `Deny`. Existing implementations remain valid.
Typed implementations may override `decide` alone; the legacy method defaults to deny
when called directly. `AllowAllApproval` remains explicit one-shot automation and does
not create session grants. Project configuration cannot enable automatic approvals.

Controlled runs use `RunHandle::respond_approval_decision(id, decision)`.
`respond_approval(id, bool)` remains a one-shot adapter. Request IDs keep run identity
and unique sequence validation; model call-ID reuse cannot redirect an answer.
A session decision without session information returns `InvalidApprovalDecision` and
leaves the question open for a valid answer. A handler returning that unsupported
choice fails closed with `approval_invalid_decision`.

The same tool preparation still runs before cache lookup, including exact edits,
revision checks and bounded diff generation. A grant bypasses the next interactive
question, not tool preconditions, prepared-plan identity, workspace checks, budgets,
cancellation, persistence or process cleanup.

Cancellation and steering are checked again after an asynchronous/ready callback,
before storing a grant. A cancelled/steered/expired question cannot install a late
permission. Grant acceptance is independent of tool success: a later edit conflict
or execution failure does not undo a broad permission the user already accepted.

## Inspection, revocation and bounds

`ApprovalSession::snapshot()` exposes session information and grant IDs/tool/capability/
registry identity. `revoke(id)` removes one grant; `revoke_all()` clears this session.
Cache `remove(workspace,id)` closes a conversation's handle, and `clear()` closes all
cached scopes. Keep scoped handles private just as you keep the cache private;
call clear/remove when ending that authority namespace rather than treating serialization
or dropping an unrelated clone as revocation.

Revocation advances an epoch and expires outstanding questions immediately, without
waiting for a human reply. Old decisions fail with `approval_revoked`, and pending
RunHandle state is cleaned up. Each permission admission checks the current scope again.
Revocation affects operations not yet admitted; it does not roll back or forcibly stop
already admitted execution. Use run cancellation when stopping in-flight work is required.
Cancelling a task does not itself revoke permissions already accepted for the session.

Default cache bounds are 32 sessions and 64 grants per session; SDK
`ApprovalCacheLimits` allows 1–1024 sessions and 1–256 grants each. Session IDs/tool names
are limited to 1024 bytes and workspace paths to 4096 encoded bytes. Session lookup
promotes recency. Eviction closes the old scope, clears grants and expires pending
questions; retained old handles fail closed. Grant overflow returns `approval_limit`
without execution or an implicit downgrade to a one-shot decision. Callers can revoke,
choose once on a fresh question, or set an explicit host capacity. These bounds cover
the cache, not arbitrary clones or overall conversation history.

## CLI, TUI and audit events

CLI shows the workspace/session and tool scope when a session is available:

- `y` / `yes`: this invocation once.
- `s`: this tool, all arguments, for this in-memory session.
- `n`, EOF or any other input: deny.

Stateless CLI `--no-session` offers y/N. TUI adds `s` to its existing y/n modal and
shows the scope above the diff. PgUp/PgDn and Tab retain diff/argument navigation.
Idle `/permissions` lists current tool grants and `/revoke` clears them. During active
work these commands ask the user to finish/cancel first; SDK hosts can revoke directly.
Successful resource reload closes old authorization scopes, while failed reload leaves
existing ones intact. Successful session deletion closes that session's scope.

`ToolApprovalRequested.session`, controlled `PendingRunApproval.session`, and
`ToolApprovalResolved.resolution` are optional event-v1 additions. Resolution records
`decision`, `source` (`handler`, `run_handle`, `session_grant`), scope and grant ID.
The legacy `approved` boolean remains. A cache hit emits an approval check/resolution
with source `session_grant`, but creates no human `RunApprovalRequested` question.
Legacy records without metadata still deserialize. Rust literals/patterns must add
new fields or `..`; see [migration](migrations.md).

The trace validator rejects contradictory decision/approved metadata, missing session
grant scope/ID and scope/session mismatch. Validation establishes trace consistency,
not current authorization. The [98-frame example trace](protocol/session-approvals-v1-trace.jsonl)
includes creation, reuse, revocation and denial. Source diffs and workspace paths remain
normal tool-trace data subject to host access/export policy.

```sh
cargo run --locked -p aicoder-runtime --example offline_approvals
```

This uses real JSONL history, atomic file tools and RunHandle decisions with a scripted
provider, and can export a full trace with an optional path argument. See
[grant verification and S3 completion](s3-approvals-acceptance.md).
