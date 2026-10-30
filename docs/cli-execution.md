# CLI execution modes (S5)

CLI input, output and cancellation now share one adapter over the core Agent. No second
model/tool loop is introduced. Existing runtime settings, providers, grants, budgets,
resources, archive windows and metadata commands remain the common implementation.

## Inputs

```sh
aicoder --prompt 'inspect the project'
aicoder 'inspect the project'
printf 'inspect this\nthen explain\n' | aicoder --stdin --print
printf 'inspect the project' | aicoder --print
```

Explicit `--prompt` or the quoted positional prompt wins as the input source. They conflict
with each other and explicit `--stdin`. Without explicit input, non-terminal stdin is read
to EOF automatically. Terminal invocation retains the historical default prompt `你是谁`.
Empty one-shot input, invalid UTF-8 and inputs above 1 MiB fail without contacting a model.
Prompt/template expansion remains runtime-owned and bounded by its existing policy.

The existing `-p` alias still means `--prompt`; final-text mode is the long `--print` flag.
Execution flags cannot be mixed with metadata subcommands. For example, `session-tree
--json` remains that subcommand's own JSON format, not an execution event stream.

## Outputs

| Mode | Stdout | Stderr |
| --- | --- | --- |
| Default | Existing content/reasoning/tool display and token summary | Diagnostics, approvals and context/source notices |
| `--print` | Only current-turn final assistant text, if present | Diagnostics/stop/session notices and approvals |
| `--json` | Exactly event-v1 JSONL frames | Logs, diagnostics, stop/session notices and approvals |

`--print` and `--json` conflict. Text output never substitutes an old historical answer
when a new turn fails before generating one. JSON emits the actual SDK envelopes, including
identity, sequence, model/round/approval events and the authoritative terminal outcome.
No plain token summary is appended. Existing event-schema consumers can validate each run.
JSON output errors or incomplete bounded observer delivery fail the process; a missing
terminal frame is not synthesized. A blocked downstream reader may receive an incomplete
prefix and a nonzero process exit rather than unlimited buffering. JSON is an event stream,
with the separate request/response [RPC protocol](rpc.md) available through `--rpc`.

Tracing is explicitly sent to stderr. Credential references/routing still come from runtime;
mode selection does not read credentials or route them through stdin.

## REPL

```sh
aicoder --repl
printf 'first question\nsecond question\n/exit\n' | aicoder --repl --print
```

Each nonempty line is one sequential turn; an optional explicit initial prompt runs first.
`/exit` and `/quit` end input and never become model prompts. EOF exits. Blank lines are
ignored. Existing runtime prompt templates/skills still expand normally; expansion errors
are displayed without sending the failed invocation to the model. Input prompts are only
shown for terminal stdin and go to stderr.

Default REPL uses the selected disk session and updates selection from each core outcome.
`--no-session` instead uses a private MemorySessionRepository so subsequent turns retain
history without creating persistent sessions/artifacts. No frontend transcript reconstruction
is used. Session grants are one ephemeral host cache shared across turns and are never
restored from recorded history. Each turn has a fresh cancellation context.

In JSON REPL output, runs are sequential complete event streams with distinct run IDs;
validators should finish/reset when the next AgentStarted arrives. Per-turn failures are
reported and the REPL can continue. Normal EOF/exit completes the REPL even after a prior
turn failure; one-shot failures exit nonzero.

## Approvals and Ctrl-C

A stdin prompt or piped REPL is payload, not permission input. These modes, plus print/JSON
execution with non-terminal stdin, deny mutation/command approval unless the user explicitly
selected `--yes`. A literal `y` in the prompt stream cannot authorize an operation. Terminal
execution reads decisions from the controlling terminal. The old explicit-prompt streaming
mode keeps its stdin approval behavior for compatibility.

Approval readers use their own nonblocking file description and check whether the question's
receiver is closed. Cancelling/expiring a question does not leave an orphan blocking stdin
thread that can steal the next REPL line or later grant authority. Frontends only return a
choice; core retains workspace/session/registry binding, revocation and dispatch rules.
Commands still run with the user's permissions and are not a sandbox.

SIGINT cancels the owning TurnExecutionContext and awaits the core terminal outcome and
journal completion. One-shot Ctrl-C exits **130** after event delivery; it does not simply
drop the execution future. Running REPL Ctrl-C cancels that turn and returns to input. Idle
REPL Ctrl-C exits input; interrupted one-shot stdin input exits 130 before model execution.
Manual compaction/model-selection commands also use the cancellable owner wrapper. Synchronous
host/filesystem work remains subject to backend behavior; signals cannot preempt a syscall.

## Evidence and scope

[Acceptance](s5-cli-acceptance.md) records checks and footprint. CLI loopback tests validate
real request bodies, final-text-only stdout, complete event traces, memory REPL continuation,
input limits, approval/data separation and SIGINT lifecycle/exit code. No real credentials
or remote model service are required. Later S5 increments add [RPC](rpc.md), [Web](web.md),
[TUI interaction](tui-interaction.md), [images](image-input.md) and [exports](session-exports.md).
Repeatable `--image FILE` captures local images for the initial prompt (including image-only
input); file/normalization reports stay on stderr. Image limits and model support are explicit.
