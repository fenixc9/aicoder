# SWE-bench generation and grading contract (S7)

Generation and official grading remain separate. The agent receives an issue, optional hints
and a repository at base_commit; gold patches, test patches, expected test IDs, eval scripts,
image metadata and future grading fields stay outside its request. The dataset preserves those
fields for hashes and later grading. Model output and tool input retain normal runtime guards.

Generation artifacts use schema 4. Grading targets the actual published **swebench 5.0.2**
wheel, rather than the moving upstream main branch. The optional package is not a Rust CLI
runtime dependency. Its official package hash covers all 65 swebench/ files; altered or
incompatible installations are rejected before a container launch. The pinned release supports
--report_dir; later upstream changes are not silently treated as this release's behavior.
See the [PyPI distribution](https://pypi.org/project/swebench/5.0.2/) and
[official harness source](https://github.com/SWE-bench/SWE-bench/tree/main/swebench/harness).

## Generation inputs, ownership and resume

Every new run writes an immutable plan.json before agent admission. It binds run ID, dataset
label, sorted selection, full per-instance hashes, request/fixture hashes, execution pin,
public parameters, model labels and concurrency. Instance identity includes hidden/future
fields as hashes, including image/eval metadata; those values do not become instructions.
Unknown or duplicate requested IDs fail before running rather than shrinking the selection.

The CLI pins its executable plus resolved runtime/global-resource settings. Embedded SDK
callers supply an explicit execution_pin over their frozen implementation/configuration.
Without a pin a fresh run is allowed, while resume is refused. Native closure and external
service state remain the host's responsibility to freeze and describe.

```rust,ignore
let options = SweBenchBatchOptions::new(output, "experiment_1")
    .dataset("pinned-local-export")
    .execution_pin(execution_sha256)
    .parameters(public_configuration);
```

A directory lease prevents concurrent generation owners. Each case writes started.json before
preparation/execution, then immutable report/trace artifacts and a terminal checkpoint receipt.
The receipt binds input hashes, prediction, usage and artifact references. Resume verifies the
plan, complete receipt, artifact bytes/hashes, request identity, captured patch and event facts.
Terminal failed cases are reused as failed cases; they do not silently rerun.

Mismatched inputs, unsupported legacy receipts, corrupt/missing artifacts or a started marker
without a complete receipt never cause automatic agent execution. Keep the old directory and
choose a fresh output for an intentional new attempt. --no-resume also requires a fresh output;
it does not delete old predictions. Interruptions before admission can leave unattempted cases,
which a later explicit resume may start after validating the same plan.

Every selected instance receives a predictions.json row, including an explicit empty patch for
setup failures or unavailable capture. Completed/incomplete/failed generation status remains
separate from whether official tests resolve its patch. A usable partial-run patch can still be
graded, with its generation stop retained. Unavailable workspace capture cannot publish a patch
as complete. Known usage survives capture failures; unknown consumption stays unknown.

`aicoder-swebench verify-generation OUTPUT/run.json` checks the whole selected table and its
artifacts. The checksum of that table is also bound into run.json. Stored display paths never
select files outside the supplied scoped output. Data is unsigned: consistency checks do not
authenticate a publisher or defend against a same-user attacker rewriting an entire coherent run.

## Host processes and Git

The reusable runtime process API accepts explicit native argv, installs no tool and grants no
model permission. It shares Bash's bounded capture, process-group ownership/signalling/reaping,
credential-reference exclusions and independent dropped-future cleanup. Host cancellation and
active deadlines return explicit stop/cleanup data. Small and binary output can be archived in
full when a scoped store is offered; UTF-8 previews and raw byte completeness are separate.

Graders, checkout, repository cache and patch extraction use this owner. Git stdout is admitted
into an 8 MiB raw store and paged back exactly; larger/incomplete output is an error, never a
truncated patch or a repeated operation. A binary patch beyond the preview bound is tested by
actual reverse/forward application. Hooks/fsmonitor are disabled for benchmark Git commands.
Credential-reference exclusions are supplied by the CLI; SDK hosts use EvalRunner's
excluding_environment method for their private reference names.

Git/cache operations have 120 s active bounds and cancellation. Cache preparation has an
async context-aware entry; old synchronous APIs retain compatibility through an owned runtime
thread. The default CLI cache is outside the fresh run output, so preparing repositories cannot
accidentally claim the generation directory. Source repositories and native tools retain host
OS permissions; this is not a filesystem/container sandbox for agent Bash commands.

## Pinned official grading

A deliberately requested optional integration can use:

```sh
python3 -m venv /tmp/aicoder-swe-harness
/tmp/aicoder-swe-harness/bin/python -m pip install -r scripts/requirements-swebench.txt
cargo build --locked -p aicoder-eval --bins --example offline_swebench
# Production grading: provide the exact local dataset export used by generation.
target/debug/aicoder-swebench grade \
  --python /tmp/aicoder-swe-harness/bin/python \
  --dataset-name /data/pinned-dataset.json \
  --predictions /data/generation/predictions.json \
  --generation-run /data/generation/run.json \
  --run-id grading_1 --report-dir /data/grading
```

The Python executable path is made absolute without resolving its symlink, preserving virtual
environment selection. -I isolates import lookup from the workspace and PYTHONPATH. A read-only
probe records Python version and verifies package version/content. The caller still selects a
trusted interpreter; metadata never selects an executable or restores model-service authority.

Linked grading verifies the generation manifest and exact prediction table, compares all
instance hashes with the supplied local dataset and materializes private canonical snapshots.
It refuses to join a mutable remote dataset name onto historical generation. Standalone
prediction grading remains available with an explicitly different scope label. Optional
--instance-id selection is validated; a subset is labelled as such, not reported as the whole
experiment. Data files and argv stay outside model requests.

Each logical grading run has a private namespace and a fresh nonce in the actual official
run ID. This avoids the upstream cache key (run ID/model/instance) mixing different patches or
colliding with another running experiment. Same complete inputs can reuse a validated grading
receipt without invoking Python or Docker. Changed patches, rows, scope, package, settings or
incomplete prior publication require a new run ID; there is no automatic relaunch.

The full official report, bounded capture, normalized score and artifact hashes are retained.
Official counts/ID sets are cross-checked against the exact selected scope. resolved_rate uses
the entire selected denominator, including empty generation-failure placeholders. The old
submitted-only ratio is available separately as submitted_resolved_rate. Generation outcome and
official resolution remain different facts. No resolved-rate claim follows from generation
completion alone.

Total harness time (default 7,200 s) is separate from per-instance test time (default 1,800 s).
On cancellation/deadline/capture failure, the Rust owner awaits process-group cleanup and then
removes only containers of its own fresh nonce/selected IDs. Removal uses immutable container
IDs, handles the official name suffix fallback, and observes a bounded quiet window. It cannot
establish that an arbitrarily delayed daemon request will never appear later; failures/uncertain
cleanup are explicit and never publish a successful score. No cleanup is authorized from saved
history or cache records, and no unrelated containers/images are removed.

CLI grading writes normalized JSON to stdout and human harness previews to stderr. SIGINT waits
for owned cleanup and exits 130. `import DIRECTORY` verifies a complete grading namespace without
Python; importing a raw official aggregate validates counts/IDs and reports its total-dataset
and submitted-only ratios separately. Raw imports do not prove a link to a generation plan.

## Offline regression and optional official fixture

```sh
python3 scripts/check_swebench.py
# Manual Docker fixture, no remote model and no public SWE-bench task downloads:
docker build -t aicoder-swe-fixture:local crates/aicoder-eval/fixtures/swebench
SWE_FIXTURE_IMAGE=$(docker image inspect --format '{{.Id}}' aicoder-swe-fixture:local)
python3 scripts/check_swebench.py \
  --official-python /tmp/aicoder-swe-harness/bin/python --image "$SWE_FIXTURE_IMAGE"
```

The example generates one correct patch, one wrong patch and one failed setup; hidden metadata
never reaches the fake provider and each admitted agent is executed once. The official Docker
fixture checks those actual files using the pinned package/parser and an immutable local image
ID. Its 1/3 fixture rate verifies the bridge, not real SWE-bench coding performance.
The manual CI workflow runs this optional protocol integration; regular CI uses the fake
harness contract and needs no swebench installation or Docker for those tests.

Defaults bound dataset input at 256 MiB / 20,000 rows / 4 MiB per canonical row, selected
generation at 5,000 cases / 16 workers, case records at 64 MiB and traces at 32 MiB. Report/trace
admission totals 512 MiB; prediction tables have a separate 256 MiB cap. Grade report/score
records cap 4 MiB. Huge or incomplete data fails closed. Image names and third-party dependency
state can remain external experimental limits; use immutable image IDs for pinned fixtures.
The [acceptance record](s7-swe-acceptance.md) distinguishes local protocol evidence from real
provider/task evaluation, Pi parity and final release work.
