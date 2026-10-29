# Bounded shell execution

Runtime's `BashTool` runs a non-interactive `/bin/bash --noprofile --norc -c` command in
the canonical workspace, with stdin closed. It remains a `Command` capability requiring
host approval. CLI, TUI and evaluation use the same runtime implementation and credential
reference exclusions. Bash has the user's filesystem/process permissions; it is not a sandbox.

## Input and host policy

Tool input remains `command` and optional `timeout_seconds` (1–86400). Omission uses
the host tool deadline.
An empty/NUL-containing command or an invalid timeout fails before spawning. Commands
are UTF-8 JSON strings; the command-size limit counts bytes. Model input cannot raise
host output or command limits. OS argv/environment size limits also apply.

`ShellLimits` is a serializable SDK policy:

| Limit | Default | Host range |
| --- | ---: | ---: |
| `max_command_bytes` | 64 KiB | 1 byte–1 MiB |
| `max_output_bytes` | 16 MiB, stdout + stderr together | 1 byte–1 GiB |

`BashTool::with_limits(limits)` validates policy and returns `ConfiguredBash`.
`excluding_environment(names)` adds host credential references. `registry_with_shell`
assembles the coding registry with a supplied shell, without changing core/provider APIs.
Hosts replacing a runtime registry must also retain any desired resource tools/policies.
Custom bench hosts should include serialized limits in their recorded parameters.

```rust,ignore
use aicoder_runtime::tools::{BashTool, ShellLimits, registry_with_shell};
let limits = ShellLimits { max_output_bytes: 64 * 1024 * 1024, ..Default::default() };
let shell = BashTool::with_limits(limits)?
    .excluding_environment(trusted_credential_variable_names);
let registry = registry_with_shell(shell)?;
let executor = TurnExecutor::builder(provider)
    .workspace(workspace).registry(registry).approval(policy).build()?;
```

Shared application settings expose `execution.tool_timeout_ms` (default 30000,
valid 1–86400000) and `AICODER_TOOL_TIMEOUT_MS`, with the usual explicit > environment >
project > user > default precedence and source inspection. Null/zero are rejected.
CLI, TUI and isolated bench factories inject this into every tool context; per-case
project settings do not replace the frozen invocation deadline. For a two-minute build:

```sh
AICODER_TOOL_TIMEOUT_MS=120000 aicoder --prompt 'build and test the project'
```

Or set `{"version":1,"settings":{"execution":{"tool_timeout_ms":120000}}}` in the
trusted/project settings layer selected by the host. The enclosing run-duration budget
can still stop execution sooner. Explicit shell `timeout_seconds` cannot extend the
host deadline. The effective value is included in runtime/bench configuration snapshots.

The shell uses Tokio IO/time/signal drivers; a current-thread runtime with `enable_all`
is sufficient. It does not require a multithreaded scheduler or add a dependency to core.
A host must retain exclusive wait ownership of these private children, rather than
reaping them through another SIGCHLD handler. Unexpected lost ownership returns a wait
error and disowns the ID instead of signalling a potentially recycled process ID.

## Capture, truncation and typed results

Stdout and stderr are read concurrently in fixed 8 KiB buffers. Each stream retains a
bounded prefix and tail; excess display data is discarded while reading continues.
A long successful build/test can finish with `truncated: true` and a visible final error
or summary line. Truncating the displayed text alone does not terminate the command.

The independent combined scan limit terminates excessive producers. At most one byte
beyond that limit is read as a sentinel. Counted bytes are **observed pipe bytes**, not
an assertion about unread bytes the process produced or would have produced later.
Raw retained data totals at most the serialized-result cap, with equal maximum storage
reserved per stream. Returned text can therefore use less than the whole result budget.
Per-stream order is retained; the two strings do not establish cross-stream ordering.
No full-output archive is silently written; artifact/reference handling remains S4 work.

`ShellOutput` is public and serializable/deserializable. A successful tool returns it as
`ToolSuccess.output`; shell execution failures attach the same shape to `ToolFailure.details`.
Validation/configuration/spawn/pipe-setup failures can lack capture details. Consumers
can use `serde_json::from_value::<ShellOutput>` instead of parsing terminal text when
the capture object is present.
The public result and `ShellStopReason` allow future extension through non-exhaustive APIs.

| Fields | Meaning |
| --- | --- |
| `exit_code`, `signal` | Observed leader status; absent when not obtained before interruption |
| `stdout`, `stderr` | Bounded text, with an explicit omission marker when trimmed |
| `stdout_bytes`, `stderr_bytes` | Raw bytes observed on each pipe |
| `stdout_omitted_bytes`, `stderr_omitted_bytes` | Observed bytes omitted from that text, including UTF-8 boundary trimming |
| `truncated` | Display data was omitted or pipe capture is incomplete |
| `output_complete` | Both pipes reached EOF; separate from display truncation |
| `stop_reason` | `exit`, `timeout`, `tool_timeout`, `output_limit`, `capture_error`, or `wait_error` |
| `process_group_signal_requested` | Group termination was requested before releasing leader ownership |
| `reaped` | This path confirmed the direct child's exit status was reaped |
| `cleanup_ok` | Group signalling succeeded/had no live target and the leader was reaped, or no child was started |
| `encoding` | `utf8_lossy`; invalid source bytes use replacement characters |

Known valid UTF-8 is not split into replacement characters merely because a retained
window ends mid-character. Binary capture is text-oriented and not a byte archive.
An exit code of zero alone does not establish tool success: an output-limit/capture/
cleanup failure may coincide with a leader that already exited successfully.

The **whole serialized output/details object**, including escaped text and metadata,
fits `min(ToolContext.max_output_bytes(), 64 KiB)`. Budgets below 1 KiB are rejected before
spawning. The outer tool/error envelope and global run-output budget remain core's
responsibility. A global budget stop can supersede an otherwise valid shell result.

## Exit, deadlines, cancellation and background jobs

The shell is a process-group leader. Exit observation uses
[`waitid` with `WNOWAIT`](https://man7.org/linux/man-pages/man2/waitid.2.html), so the leader
remains owned and waitable while its group is signalled. Reaping happens afterward;
cleanup never intentionally sends a group signal using an ID already released by this tool.
Normal zero/nonzero exit also terminates same-group background work. Commands that need
background jobs to finish must explicitly `wait`; background daemons do not survive by default.
Closing stdout/stderr alone is not mistaken for process exit.

The command deadline covers active execution. Group termination is followed by up to
250 ms for leader reaping and up to 250 ms for pipe draining within the remaining scan
budget. Inherited pipes cannot hold a completed command open indefinitely. The separate
host tool deadline caps the entire call, including this cleanup. Direct host-deadline
failures retain captured output and mark unconfirmed cleanup/reaping explicitly.

On future drop (including RunHandle cancellation, core timeout or runtime shutdown),
the guard requests SIGKILL synchronously. If needed, a short-lived OS waiter owns only
`Child::wait`; it performs no tool work and does not need a Tokio driver. It exits once
the direct child is reaped. Normal confirmed completion creates no reaper thread. Thread
creation/resource exhaustion is reported to tracing; kill requests are still made first.

Core cancellation/duration/tool deadlines may win before Bash returns its own failure
object. Those paths retain core's existing `RunOutcome` and journal semantics; current
shell output is not guaranteed to become a new tool result. Earlier checkpoints remain
available. Commands may already have modified the workspace before interruption; inspect
it before retrying an uncertain operation.

Process groups do not contain deliberately detached sessions/groups. SIGKILL cannot
complete uninterruptible kernel work on demand, and arbitrary child memory/CPU/disk use
is not quota-controlled by this tool. `cleanup_ok` describes signalling and direct-child
reaping, not proof that every descendant in the system has disappeared. Linux is the
validated platform; no Windows/job-object or macOS equivalence is claimed.

## Credential and startup environment

The child retains normal development environment such as PATH and HOME. It removes
managed provider variables (`OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, `AICODER_API_KEY`,
`ANTHROPIC_API_KEY`, `DASHSCOPE_API_KEY`, `ZHIPU_API_KEY`), the selected
`AICODER_API_KEY_ENV` reference and router variable, `AICODER_PROXY`, and every trusted
runtime profile credential reference, including inactive profiles.

Startup/control exclusions are BASH_ENV, ENV, SHELLOPTS, BASHOPTS, CDPATH, GLOBIGNORE,
BASH_XTRACEFD, BASH_COMPAT and POSIXLY_CORRECT. Exported `BASH_FUNC_…` environment entries
are removed by name. This prevents inherited startup files/functions/options from
changing the invocation implicitly; see the [Bash startup/environment rules](https://www.gnu.org/s/bash/manual/bash.html).
The implementation does not enable Bash privileged/restricted mode or change user IDs.
Names are validated without echoing invalid credential values into errors.

These exclusions are not general secret-value redaction or an environment allowlist.
Unregistered credentials in other variables and files are still accessible with user
permissions. The ordinary development environment remains available, and explicit
commands can load their own files. Automated fixtures use fake values and do not dump
real process credentials.

```sh
cargo run --locked -p aicoder-runtime --example offline_shell
cargo run --locked -p aicoder-runtime --example offline_shell -- --capture-bytes 1048576
```

The first example exercises real shell calls in a scripted three-round model loop;
the second measures bounded capture without printing the large log. See
[verification and measurements](s3-shell-acceptance.md) and [migration](migrations.md).
