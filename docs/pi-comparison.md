# Manual Pi comparison and first Spark smoke

`scripts/compare_pi.py` runs the same four coding_v1 tasks through aicoder's direct
core/runtime benchmark and the locally installed Pi 1.4.0 JSON entry. It uses fresh workspaces,
disables project resources/extensions/MCP/session restoration, alternates agent order by case,
and keeps hidden grading outside model input. Pi events remain Pi events; no synthetic core
history is fabricated for an external agent. No production Rust dependency changes.

The default is a scripted loopback integration fixture. A real service requires explicit
`--live` and an existing trusted Pi model configuration. The selected Spark credential stays
inside the comparison host and is added only to upstream HTTP headers. Agent processes receive
only an isolated loopback credential; graders need no service credential. Existing Pi
settings/auth files are not edited.
Command credentials are not executed by this development tool. Current execution requires
Linux Bubblewrap with user namespaces and a working pinned Rust/linker toolchain; failed
isolation/preflight never falls back to host execution or starts a model request.

```sh
cargo build --locked -p aicoder-eval --bin aicoder-bench
python3 scripts/compare_pi.py --output /tmp/new-offline-comparison
python3 scripts/compare_pi.py --verify /tmp/new-offline-comparison
python3 scripts/check_pi_comparison.py
python3 scripts/check_pi_isolation.py
```

The offline acceptance needs installed Pi 1.4.0, Node and Bubblewrap, but no model credential or remote
service. It checks five complete cohorts: correct fixture, noop, provider error, unknown usage
and excessive tool batch. Every cohort retains all four selected tasks per agent. Invalid
artifact hashes, symlinks, FIFOs, replacement writes and exhausted collection bounds reject;
capture drains both pipes after preview exhaustion, waits after EOF, and reaps its owned
leader after timeout. Verification works without Pi/Rust/provider executables in PATH.

## Resource and evidence contract

The loopback adapter sends both agents' requests to the same selected model with seed 0,
temperature 0, at most 2048 output tokens and no streaming. Pi receives a conversion to SSE;
this does not compare real service streaming behavior. Original system prompts/tool contracts
remain different. Aicoder uses its SDK benchmark executor; Pi uses its standard coding-session
loop. Identical service options do not imply identical requests or deterministic output.

Each task admits at most 8 upstream model attempts, 16 returned tool calls, 32,000 observed
total tokens and 120 seconds of model/engine time. Aicoder additionally has its native core
preflight limits. Completed upstream usage is charged even after frontend cancellation. Token
usage becomes known only after a response: a last charged request can exceed the token cap,
and unknown usage prevents another request. This is not a hard monetary spending ceiling.
All original outcomes remain in the denominator; completed patches from an incomplete run
do not become successful completed runs.

Output is a fresh private directory. Staged files publish without replacement; plan, stdout,
stderr, parsed provider requests/responses, case results and applicable coding archives have
byte/hash references. The host collection caps files at 32 MiB and admitted evidence at
128 MiB; nested coding archives use their existing independent disk limits before collection.
An exhausted collection leaves forensic data without a final comparison marker and never
repeats completed work. Verification bounds reads through directory descriptors, validates
references and derives the selected summary; it executes no agent/grader. Hashes are unsigned
integrity evidence, not proof of a trustworthy publisher or model performance.

The current runner copies the complete Pi installation (including directories and safe internal
links), Node and aicoder into a private frozen snapshot, recording each file's content hash
and executable bit before launch. Inputs cap at 32,768 entries / 512 MiB of file/link bytes;
the canonical entry list caps at 4 MiB. Private .env files, special files, escaping links, changed
files and exhausted disk admission reject. Code mounts are read-only; later edits of the
installed source do not change a running generation. The plan pins the execution manifest,
source observation, suite and both comparison/isolation scripts. Compiler identity is checked;
the read-only host toolchain/system libraries are not a complete hermetic OS image pin.

Each engine gets a new PID namespace, dedicated /tmp tmpfs and read-only system/application
mounts. Only its own case workspace or output directory is writable on the host. The source
checkout and user's home/credentials are not mounted. Pi's hidden Rust compilation/test run
also gets separate namespaces after the agent exits. Namespace owner death stops detached
descendants too; adopted zombies may still await the host init's reaper. Host networking is
shared for the explicit loopback model proxy; this is not a network, kernel, memory or complete
disk-quota sandbox. Temporary filesystems use the kernel's default size on Bubblewrap 0.6.1.

`aicoder-bench observe-source --source-root ROOT` captures source/compiler/executable metadata
before isolation. `run --source-observation FILE --source-root /app` checks a bounded trusted
observation against the current executable/platform/compiler without mounting source. The host
owns the source-content claim; this metadata does not restore IO, model routing or grants.
Default runs still capture source directly, and existing archive formats remain readable.

The first smoke below predates these changes: its plan pinned only the Pi CLI wrapper, and
its supplemental bundle hash was observed after execution. Its results are preserved with
those original limitations, not retrospectively relabelled as isolated or fully pinned.

Current root-process RSS samples the Bubblewrap launcher every 10 ms, **not the agent**.
It cannot compare agent memory; the separately scoped internal aicoder observation remains
available in its coding archive. The historical unisolated smoke sampled actual root engines,
with different grading/startup scopes. Agent duration comes from core for aicoder and observed
stdout events for Pi; neither scope difference supports an exact latency/memory comparison.
Unknown prices stay null; Pi's zero-price configuration placeholders are not billed cost.

## Observed Spark result

The [public experiment summary](baselines/s7-spark-pi-smoke.json) retains all eight original
outcomes, input/code hashes and limitations. Raw archives remain local. This was a manual
exploratory run on `spark / DeepSeek-V4.1-Flash-1017`, pinned Rust 1.92.0, Pi 1.4.0 and
Node 22.19.0, with one sample per task and the limits above.

| Task | aicoder | Pi |
| --- | --- | --- |
| Inclusive range | Completed, tests passed | Completed, tests passed |
| Checked clamp | Tests passed; incomplete at 8 rounds | Completed, tests passed |
| Unicode suffix | Completed, tests passed | Completed, tests passed |
| Retry JSON | Completed, tests passed | Completed, tests passed |

Successful completed runs are **3/4 for aicoder and 4/4 for Pi**. Hidden repair tests passed
4/4 for each. The incomplete aicoder case made 9 tool calls with 3 tool errors and exhausted
its round budget; it is retained as inconclusive. Across the four cases aicoder used 24 model
requests / 53,366 reported tokens / 6 tool errors; Pi used 18 / 34,367 / 1. Total observed usage
was 87,733 tokens; actual monetary cost is unknown. No real-service retries were added and
no failed task was rerun to select a better score.

This first run used the existing **non-sandboxed host Bash** authority. Model shell commands
also wrote shared /tmp paths outside case workspaces, including common test directories.
Cross-case filesystem isolation was therefore not established. Tool execution must be isolated
before expanding or accepting a final comparison. This run is preserved as exploratory evidence,
not final equal-environment acceptance or a general coding-success benchmark. The next work
was isolated execution and complete Pi package pinning. Those paths now pass offline
contracts; a larger isolated real-service paired sample remains outstanding.

Full feature parity remains tracked separately in the [Pi matrix](pi-parity-audit.md).
The isolation/source-observation checks and default footprint are recorded in
[S7 isolation acceptance](s7-pi-isolation-acceptance.md).
