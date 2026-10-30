# TUI interaction (S5)

The TUI adapts core Agent/RunHandle and shared runtime settings/tools/sessions. It owns
editing, history, display and theme state, not a second model/tool loop or permission policy.

## Composer

Enter submits; during a run it steers, while Alt+Enter queues follow-up. Shift+Enter or
Ctrl+O inserts a newline (Ctrl+O works on terminals without modified-Enter reporting).
Recognized bracketed paste inserts text atomically, preserving newlines; the paste event never submits commands
or answers an approval/deletion dialog. Drafts are bounded to 64 KiB; oversized paste fails
without changing the draft. Unicode grapheme edits and display-cell wrapping keep multiline
cursors inside the input viewport. Home/End address the current line; Ctrl+Home/End address
the whole draft. Up/Down move by visual rows within multiline input, then recall history at its edges;
Ctrl+P/N explicitly recall previous/next. Returning past the newest entry restores the draft; editing a recalled entry starts a new draft.

History retains at most 100 accepted raw prompt strings / 256 KiB in memory. It does not
persist a separate log, include approval answers, replay commands automatically, re-expand
old recorded context or restore file attachments/authority. Failed submissions preserve
raw input and pending references. Canonical conversation history remains core-owned.

## Explicit file pages

`/file PATH` reads one workspace-scoped UTF-8 page through runtime's existing read_file
backend (16 KiB serialized page budget, bounded scan/timeout and unchanged-file check).
It stages an immutable user attachment; `/detach [INDEX|all]` removes staged pages.
At most four pages / 64 KiB may be staged. The next accepted turn/steer/follow-up includes
the captured page JSON, path/revision, completeness and usable continuation cursor as user
data. Files are not re-read on submit or historical replay. Partial pages are labelled;
subsequent read_file use rejects stale cursors. Attachment errors/cancellation preserve the
draft. Session navigation clears unsubmitted references. Binary, external workspace paths
and unsupported file types fail rather than silently truncating or traversing outside scope.

A reference is explicit host/user input, not an invented tool result or system instruction.
It does not execute commands or bypass core dispatch approvals for later model/tool actions.
Core control input byte/queue limits still apply after template/attachment expansion. Large
queued messages can be rejected and remain available for editing.

## State, context and cost

The header reports current run state, model, requested reasoning, usage and elapsed time.
The second line shows the latest projected request input count, declared host/model window,
scope, precision and method. Percentages cover input only; reserved output and billing are
separate. Unknown windows/counts remain unknown. Live count metadata comes from a bounded
core control snapshot, never from frontend token estimation or repaired history. `/usage`
shows complete available counting/fallback metadata and estimate availability.

A base-rate cost estimate is optional: it needs declared validated model prices, a finished
run with complete reported input/output usage, no estimated attempts, no summary attempts,
and no reported cache writes/reads requiring undeclared rates. Unknown prices/usage or mixed
summary work show unavailable. It is labelled an estimate, not a billed amount. Browsing
aggregate historical branch usage does not apply one current model's price to past models.
No network price discovery is performed.

## Themes and boundaries

`--theme dark|light|mono` and `/theme NAME` select frontend-only palettes. `--theme-file PATH`
loads an explicit bounded (4 KiB) JSON file with optional base plus #RRGGBB colors for border,
accent, user, tool, error, muted, foreground and background. Unknown fields fail. Theme data
never enters runtime/core settings, prompts, session authority or provider requests.

Rendering escapes terminal controls in untrusted text while retaining raw canonical data.
Visible transcript previews/history caches are bounded and label omitted material; reopen
available core/archive/artifact history to inspect full data. Persistence failures remain
explicit; clipping is not a guarantee that failed archive writes can be recovered. Native session/model/branch/summary/
permission commands and diff approvals still use existing core/runtime services. Ctrl-C/Esc
cancel through the owning context; terminal errors/EOF await owners before runtime teardown.
Recognized paste events cannot authorize a plan, and UI themes cannot change permission rules.

[Acceptance](s5-tui-acceptance.md) records unit, rendered-backend and actual PTY evidence.
Image attachment input is a separate remaining S5 item; `/file` is a bounded text reference.

Terminal backends can buffer a complete paste event before the composer validates its size.
The 64 KiB limit bounds accepted drafts; it is not a transport-level memory ceiling for an
arbitrarily large/incomplete terminal event. Legacy unbracketed text is indistinguishable
from typed keys and cannot inherit bracketed-paste guarantees.
