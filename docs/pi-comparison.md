# Manual Pi comparison and first Spark smoke

`scripts/compare_pi.py` runs the same four coding_v1 tasks through aicoder's direct
core/runtime benchmark and the locally installed Pi 1.4.0 JSON entry. It uses fresh workspaces,
disables project resources/extensions/MCP/session restoration, alternates agent order by case,
and keeps hidden grading outside model input. Pi events remain Pi events; no synthetic core
history is fabricated for an external agent. No production Rust dependency changes.

The default is a scripted loopback integration fixture. A real service requires explicit
`--live` and an existing trusted Pi model configuration. The selected Spark credential stays
inside the comparison host and is added only to upstream HTTP headers. Agent processes and
graders receive an isolated loopback credential. Existing Pi settings/auth files are not edited.
Command credentials are not executed by this development tool.

```sh
cargo build --locked -p aicoder-eval --bin aicoder-bench
python3 scripts/compare_pi.py --output /tmp/new-offline-comparison
python3 scripts/compare_pi.py --verify /tmp/new-offline-comparison
python3 scripts/check_pi_comparison.py
```

The offline acceptance needs installed Pi 1.4.0 and Node, but no model credential or remote
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

The plan pins the aicoder executable, source revision, suite and comparison script. The initial
Pi pin includes installed version and CLI wrapper hash, not all runtime chunks/assets. The
first smoke also records a supplemental bundle digest observed after execution; this does
not establish a complete immutable package before launch. A final release comparison needs
stronger Pi execution pinning.

Root-process RSS is sampled every 10 ms and excludes children/proxy/other processes. Aicoder's
process includes its grading/archive host; Pi's external grading is outside its process. Agent
duration comes from core for aicoder and observed stdout events for Pi. Those scopes and
startup differences prevent treating these numbers as exact isolated agent memory or latency.
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
is isolated execution and complete Pi package pinning, followed by a larger paired sample.

Full feature parity remains tracked separately in the [Pi matrix](pi-parity-audit.md).
