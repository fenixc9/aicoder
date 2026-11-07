# Compaction interception and observation

`Extension::compaction_hook(HookSpec, Arc<dyn CompactionHook>)` registers an immutable native
before/after pair for automatic and manual compaction. It works through the existing Agent,
journal, counting policy and storage backends. No callback is installed by default.

Before callbacks receive the trigger (Automatic or Manual), core model context, canonical history,
declared runtime prefix, optional session IDs, input target, earlier decisions and scoped model
work. Priority/installation order follows other native hooks. They can:

- Return no action to retain default behavior.
- Return `cancel: true` to veto this compaction. A veto ends the chain and performs no default
  summary request or reduction checkpoint. Manual reports cancellation without cancelling its
  host context. Automatic generation stops explicitly; it cannot bypass the requested context
  policy by sending an unapproved view.
- Supply `SummaryInstructions { text, replace_default }` for the default semantic strategy.
  Append mode adds host guidance. Replacement mode replaces default content guidance while the
  mandatory JSON schema, tool-free framing and quoted-data protections remain. Instructions are
  bounded to 32 KiB and stay in ephemeral requests; receipts retain sizes/source, not their text.
- Supply `CompactionReplacement { summary, first_kept_entry_id, details }` as a native reduction.
  Exact IDs retain a complete raw suffix; None keeps the summary and later facts. Details remain
  inert. A later hook can replace this result or set `use_default: true` to resume generation.

The complete staged decision is bounded to 64 KiB, cumulative chain data to 256 KiB. Calls use
HookSpec timeouts and a 30-second phase bound, with the parent elapsed budget throughout. Every
managed helper request consumes the same parent auxiliary/token/concurrency limits; unknown usage
uses estimated reservation accounting. `input.models.generate` accepts an explicit host provider
object and a bounded tool-free request, as in [boundary model work](boundary-drafts.md). Its phase
label is compaction. Routes, credentials, grants and backend handles never come from stored data.

Native replacements use the existing one-use `PreparedBoundary` / `SessionAppendPlan` protocol.
The full or admitted window projection is prepared read-only, keeps the runtime prefix, passes
adapter/capability checks, actually reduces measured context and fits the requested target. The
exact owned append is acknowledged before adopting the reduced view. Invalid replacements fail
without a default-service fallback; they cannot hide an already completed auxiliary request.
Window targets must be real admitted records. Model work/metadata receipts may already exist when
an uncommitted replacement is refused; original user/tool records remain unchanged.

Default generation keeps the configured compactor and its counted/projected API. The additive
`ContextCompactor::compact_customized` method defaults to the old projected method when no
instructions exist, and rejects unsupported instruction customization before model IO. The built-in
semantic strategy includes instruction identity in failed-source caching, counts the actual system
prompt, and retains known usage on output admission errors. No-hook automatic execution follows
the existing path without a hook snapshot or extra context copy.

Manual compaction uses an idle SessionJournal under the original lease; it creates no fictitious
TurnStart/TurnEnd records. Model work is journaled as real auxiliary observations. Interrupted
helper attempts are settled into typed Finished receipts after cancellation when storage permits,
so known partial usage remains visible even without terminal turn statistics. Default semantic
usage belongs to its checkpoint or manual usage record, while helper usage belongs to its own
receipts. Reports expose aggregate usage, optional primary subtotal and auxiliary attempts without
double accounting. The default service receives the token budget remaining after before callbacks.

`after` receives only a safe `CompactionEvent`: trigger, status, whether replacement content came
from an extension, checkpoint ID and token-size observations. Completed, Unchanged, Vetoed,
Cancelled and Failed outcomes are distinct. It has no managed model-work/authorization interface
and cannot veto or rewrite an acknowledged checkpoint. Delivery counters and sanitized failure
classes remain separate. Deadlines expire observers/UI children; no completed provider work is
retried because an observer or observation receipt fails. A committed checkpoint ID survives later
budget/cancellation or receipt failure. TUI/CLI presentation identifies an applied checkpoint
separately from incomplete post-compaction work; manual cost display stays conservative.

Automatic execution publishes context completion before awaiting post-effect observations and
checks elapsed budgets/controls before main provider IO. New steering accepted during the phase
uses the existing input journal. Before/after provenance uses `aicoder.compaction-hooks.v1` inert
records; guidance is not restored from them. Native replacements keep their native reduction
namespace, and semantic checkpoint v1/v2 rules remain unchanged. A journal must support the new
hook receipt probe before installed hooks perform provider work; `()` and EvaluationJournal
explicitly support the ephemeral contract. Native code remains trusted host code.

Run [the offline example](../crates/aicoder-runtime/examples/offline_compaction_hooks.rs):

```sh
cargo run --locked -p aicoder-runtime --example offline_compaction_hooks
cargo run --locked -p aicoder-runtime --example offline_compaction_hooks -- --manual --window
cargo run --locked -p aicoder-runtime --example offline_compaction_hooks -- --manual --veto
cargo run --locked -p aicoder-runtime --example offline_compaction_hooks -- --instructions --window
cargo run --locked -p aicoder-runtime --example offline_compaction_hooks -- --manual --observer-fail
```

Examples use scripted providers and real JSONL/window backends without external requests or
credentials. [Acceptance](s7-compaction-hooks-acceptance.md) records exact checks. Tree-summary
customization and final isolated Spark/Pi/SWE quality/performance/release acceptance remain open.
This native API does not reproduce Pi's TypeScript wire/ABI or declare an overflow-retry behavior
that the current host does not execute.
