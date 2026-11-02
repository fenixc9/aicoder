# Reproducible coding benchmark archives (S7)

`aicoder-bench` runs isolated coding cases directly through EvalRunner, TurnExecutor and runtime
tools. It records a versioned archive rather than parsing CLI/TUI output. The built-in coding_v1
suite has three small Rust repairs (inclusive bounds, checked clamp, Unicode delimiter handling)
and one semantic JSON edit. Its default provider is a fixed in-memory contract fixture.
Passing that provider proves the execution/grading/archive path; it does not measure LLM coding
ability. Actual service calls require an explicit `--live` invocation.

## Run, verify, replay and compare

```sh
cargo build --locked -p aicoder-eval --bin aicoder-bench
mkdir -p artifacts
# Existing output directories are rejected before any agent call; choose a fresh directory.
target/debug/aicoder-bench run --output artifacts/coding-left
target/debug/aicoder-bench verify artifacts/coding-left
target/debug/aicoder-bench replay artifacts/coding-left --case inclusive_range
target/debug/aicoder-bench run --output artifacts/coding-right
target/debug/aicoder-bench compare artifacts/coding-left artifacts/coding-right
python3 scripts/check_benchmark.py
```

`list` shows task IDs, prompts and initial filenames without exposing graders. `--case ID` is
repeatable; `--repetitions` accepts 1–8. `--suite PATH` loads an explicitly trusted, bounded
suite JSON following CodingSuite/CodingCase/Grading. This file contains trusted hidden Rust
harnesses and scripted fixture answers: do not load an untrusted suite or place it in the
Agent's workspace. The suite is pinned by a SHA-256 covering its canonical serialized content, including
grading and fixture responses. Suite parsing rejects path traversal, reserved paths, duplicate
IDs, oversized files/prompts and excessive case counts before executing a case.

For a deliberately requested service smoke, configure the existing provider route with a
trusted user config/environment file, select the model and keep comparison flags fixed:

```sh
target/debug/aicoder-bench run --live --model MODEL \
  --user-config /trusted/provider-settings.json \
  --output artifacts/manual-service-smoke --case inclusive_range \
  --max-rounds 8 --max-tool-calls 16 --token-budget 32000 \
  --time-budget-ms 30000 --output-budget-bytes 1048576
```

User config/environment-file options require `--live`. Offline runs inject an empty settings
environment and never construct a model-service client. Live mode uses normal runtime
credential references; values do not enter the manifest. Project resource/package discovery
and session directories are disabled in this entry. SDK hosts can supply a frozen runtime
with resources/extensions through their executor factory; actual resource provenance and
registrations are captured. Model options retain unset values; a supported, otherwise unset
seed becomes 0. Unsupported seed requests fail through normal adapter validation.

Run exits 0 only when every selected case passes, 1 for failed/incomplete/inconclusive cases,
and 130 after an owned SIGINT cancellation is recorded. Remaining unstarted cases stay in the
manifest. No implicit resume or retry is installed. A completed operation is never repeated
because capture or publication failed. An interrupted directory without manifest.json is an
incomplete archive; do not treat it as successful or reuse it as a new run directory.

## Execution and grading boundary

Each case receives a fresh temporary copy of its declared initial files. Its model request
contains the task prompt and host system prefix. Hidden tests, expected JSON and scripted
answers are not request inputs or project resources. Runtime tools retain their real path,
prepared-plan, output, cancellation and approval contracts. This CLI explicitly installs
AllowAllApproval for its isolated benchmark tasks; normal application approvals are unchanged.

Once the agent owner ends, Rust grading creates a separate temporary directory, copies the
captured candidate Rust files, writes the hidden test harness and pinned rust-toolchain.toml,
then compiles/runs the tests. JSON grading compares parsed values, allowing different valid
formatting. Alternative correct Rust implementations can pass the same behavior checks.
Grader output is never fed back as a new model turn. Same-user Bash/native code retains OS
permissions; this separation is not an OS/container sandbox or a guarantee against deliberate
benchmark cheating. Official SWE-bench Docker grading remains a separate workflow.

Trusted grader processes reuse runtime Bash's bounded capture, credential exclusions and
owned process-group cleanup. Arguments are shell-quoted literals. UTF-8 arguments, command
size, active time and output scanning have limits. Timeout/capture/cleanup failures are visible;
no incomplete capture is labelled successful. Dropped futures signal group cleanup independently
of the async runtime. Actual cleanup flags appear in timeout findings.

A completed but ungraded EvalRunner run is Inconclusive. Runtime failure cannot become Passed
without a grader. Overflowed observation becomes an evaluator error. If the post-run workspace
snapshot fails, the report retains actual core progress/usage/trace and sets
workspace_capture_complete=false. Its placeholder empty path list is unavailable data, not a
claim that no files changed. Snapshots record symlink targets without reading through them,
include executable-bit changes, and reject special files, changing files and exhausted bounds.

## Archive format and evidence

manifest.json is the final publication marker (schema_version=1, event_schema_version=1).
It contains the suite hash, source revision/dirty state, Rust/manifests/embedded-assets hash,
actual executable hash where available, observed grader compiler version/platform, explicit
mode/selection/repetitions, public host parameters, case summaries and artifact references.
Each case records:

- Actual request hash/options, model capabilities, installed execution/dispatcher limits,
  tool schemas/capabilities, extension registrations, context policy, initial workspace fingerprint and
  observed resource input provenance in report.json. These are execution observations;
  metadata does not restore grants, backend tokens or provider-routing authority.
- Core outcome/stop/progress, grader verdicts and errors. Factory, provider, budget, cancelled,
  ungraded and artifact failures all remain in the selected denominator. Unknown consumption
  remains unknown; failures cannot silently reduce the denominator or become exact zero cost.
- trace.json with the ordered typed event-v1 envelopes; workspace-diff.json and, for the
  built-in UTF-8 fixtures, a complete applicable workspace.patch. Binary/non-UTF-8 non-Git
  changes have no text patch; the path diff and capture-completeness fields remain explicit.
- Root agent duration and whole-case duration including preparation/grading. Memory samples
  Linux /proc/self/status every 10 ms and at endpoints while EvalRunner prepares, executes
  and grades the case: scope is the benchmark process including those harness allocations.
  Later archive serialization/publication and child/tool/compiler memory are excluded. This is sampled
  RSS, not exact peak, allocator accounting or an isolated per-agent memory bound.

Each artifact has a scoped relative path, exact serialized byte count and SHA-256. Linux
archive IO anchors directory file descriptors and uses no-follow opens, so replacing a leaf
or parent with a symlink cannot redirect a read/write outside the selected storage. Files
are created exclusively; final manifest publication cannot replace an existing manifest.
References claim only fully written files. Artifact errors retain known metrics and available
forensic files, lower the verdict, and never claim success from a partially archived case.

`verify` bounds reads, checks references and path ownership, validates event lifecycles,
compares terminal usage/tool/round/stop fields with report metrics, checks diff/patch consistency
and derives the summary again from every selected case. It rejects mismatches and incomplete
success claims. Hashes detect inconsistent data; archives are unsigned and do not authenticate
a publisher or prove that a same-user attacker could not rewrite an entire consistent archive.
`replay` verifies first and returns the recorded trajectory without any provider/tool/grader
execution. Incomplete observation cannot be replayed as a complete trace.

`compare` verifies both archives and requires equal suite/selection/repetition/mode/public
parameters, observed grader compiler and actual per-case input identities. Input identity
covers request, model/options, installed bounds, tools/extensions, context policy, initial
workspace and input-resource metadata; only current temporary-workspace paths are normalized.
Unknown model inputs or mismatches reject comparison. Native closure state and external service
state are not introspectable; SDK hosts must freeze them and describe public parameters explicitly. Source/binary revisions may differ
because comparing execution changes is intentional. Success comparisons retain all failure
kinds; they do not attribute a difference to model quality rather than infrastructure.

Cost stays null for scripted providers, unknown prices/usage, cached usage without rate detail
or semantic-summary requests using other model rates. Known base-rate estimates are labelled
as estimates and never billed cost. Platform/load, OS caches, service nondeterminism and a
provider's treatment of seeds remain experimental limits.

## Bounds and scope

Default trace retention is a contiguous prefix of at most 100,000 events / 32 MiB,
including JSON array framing. The legacy trace writer also uses bounded staging and atomic
publication, preserving existing output on serialization failure. After the
first missing event, later events are counted as dropped, not spliced into a complete stream.
Evaluation journals cap 4,096 records / 8 MiB, including real composition facts and public
transient store commits. They never persist live authority. Workspace capture/copy defaults
are 20,000 scanned entries, 16 MiB/file, 128 MiB total and depth 64. Snapshot/diff budget failures
are errors, never silent pruning. Non-Git text diff rendering caps 8 MiB / 250 ms.

Suites cap 32 cases, 64 initial files/case and 1 MiB encoded data. Archives cap 256 selected
case/repetition pairs, four references/case, 64 MiB/file, 512 MiB admitted artifact bytes and a
4 MiB final manifest. Coding grader compilation/execution use 30 s / 5 s limits and bounded
prefix/tail previews; source Git metadata and legacy SWE-bench clone/patch preparation remain
host-side operations with their existing IO behavior. This work does not claim to bound those
legacy checkout operations or replace official grading. The currently verified archive backend
requires Linux /proc/self/fd; other platforms return an explicit unsupported error.

The executable tests and acceptance evidence are in [S7 benchmark acceptance](s7-bench-acceptance.md).
Real-provider coding success, the identical-budget Pi run, full feature-matrix audit and release
acceptance remain open. Neither these contract scores nor one manual smoke replaces that work.
