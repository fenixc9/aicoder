# Session lifecycle and guarded navigation

Native hosts register Extension::session_hook(HookSpec, Arc<dyn SessionHook>). Before callbacks
inspect an immutable operation and source/proposed-target positions, then Continue or Deny.
They receive no repository, backend, lease, model-route or approval authority. Priority ordering,
bounded registration and private callback/UI cancellation follow the other native hooks.
After callbacks observe completed changes; failures report incomplete delivery without changing
the adopted state or repeating effects. These operations perform no model generation.

SessionNavigator::prepare resolves labels/entries, freezes the exact transaction and retains the
existing run lease. Agent::apply_navigation accepts that owned plan; navigate_session performs
both steps. Checkout/branch projections are validated against the explicit host provider/model
before guards and again before commit. Rename/label metadata does not require model compatibility.
Recovery retains its existing canonical/uncertainty rules. A voted plan is never refreshed after
callback work. Conflicts, malformed targets, denial, cancellation and guard timeout reject before
mutation; dropping preparation releases ownership. Model selection previews remain tool-free.

Full snapshots and bounded JSONL windows share the transaction builder. Window preparation uses
private archive descriptors and real materialized records; it never converts descriptors into
history or model instructions. Backend fingerprints and conflict checks remain active at commit.
Plans/previews are not serializable. Old branches remain available, and planned marker IDs match
the eventual committed IDs. Completion observation retains ownership until its bounded wait ends.

Explicit Agent::select_session handles host startup/new/resume, with source/target leases and
compatibility validation before adoption. It guards New before creating a file. Successful
replacement observes Shutdown and Started; selecting the same binding is a no-op. Reports use
Startup/New/Resume reasons. If creation succeeds but ownership cannot be acquired, the typed error
retains the created session ID for inspection; it never silently creates another file.

run_hosted_session_outcome / controlled add explicit adoption around the existing Agent turn API,
returning both its outcome and bounded lifecycle reports. Host-owned current IDs distinguish
continuation from adoption. A private clock charges preparation/guard/observation waits to the
same duration budget; expiry before a write prevents it. Expiry/cancellation after acknowledgement
retains actual metadata and reports incomplete observations. Ordinary Existing turn APIs represent
continuation, and lower-level storage APIs remain explicit trusted host primitives.

notify_session_shutdown runs only after the owner settles and cannot veto exit. Resource reload
notifications follow successful runtime adoption, reporting Shutdown/Started with Reload reason.
Reload retains the existing executable generation and immutable native registrations; it does not
load native code from history or implicitly restart programs. These notifications do not delete
sessions, grant tools or reconstruct queues. Hosts must await the owner after cancellation.

Before/after phases are bounded to 30 seconds, with HookSpec limits and parent duration checks.
Late synchronous callback results are rejected before commit. Session observation data is bounded
to 64 KiB per event; delivery reports distinguish registered, actually invoked and completed
callbacks, with safe failure classes rather than raw callback errors or private views. Positions
contain IDs/revisions/leaves/titles, not storage paths or executable handles. Reports are inert
operator data and are not appended to model context or canonical history.

Stock CLI metadata actions use the guarded API and retain their stdout format. RPC owns one
pending native selection, keeps status/cancel/shutdown/EOF responsive and awaits its task before
transport teardown. cancel_session_selection carries the original request ID; stale IDs cannot
cancel another operation. Native results optionally include session_lifecycle reports. Web authenticates
before bodies, uses the hosted run path, and sends one shutdown notification after owner cleanup.

TUI mutation/new/resume operations run asynchronously with private UI support. Paste is payload,
never a choice or automatic submission. Denial preserves the current view/draft/authority; a
successful response adopts committed metadata even if cancellation raced post-commit observation.
Deleting the active stored session first adopts a guarded replacement; later deletion failure
reports that partial result. Grant retirement follows adoption, not a rejected proposal. Native
startup veto cannot be bypassed by a merely previewed session ID. Reload/exit await their owners.

```sh
cargo run --locked -p aicoder-runtime --example offline_session_lifecycle
cargo run --locked -p aicoder-runtime --example offline_session_lifecycle -- --window --deny
cargo run --locked -p aicoder-runtime --example offline_session_lifecycle -- --observer-error
```

These scripted examples use real full/window JSONL navigation with no model credentials. The
terminal fixture's --session-hooks mode exercises a private before-rename choice. See
[acceptance](s7-session-lifecycle-acceptance.md). This increment does not implement cross-session
fork/clone, tree-navigation summaries, before-compaction replacement or boundary context-edit /
compaction drafts. Those capabilities and final real-model acceptance remain in the [audit](pi-parity-audit.md).
