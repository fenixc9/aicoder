# S7 SWE-bench generation/grading acceptance

[SWE-bench schema-4 contracts](swebench-contract.md) now bind full generation inputs and owned
receipts, keep every failed generation in the selected denominator and verify the official
5.0.2 package before grading. This closes the generation/grading isolation item; it does not
claim real-model SWE-bench success, Pi coding parity or completed release acceptance.

## Verification

Passed on pinned Rust 1.92.0 / Linux x86_64:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p aicoder-core --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo run --locked -p aicoder-runtime --example offline_smoke
cargo run --locked -p aicoder-eval --example offline_swebench
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-eval --features server --bins --examples
python3 scripts/check_benchmark.py
python3 scripts/check_swebench.py
python3 scripts/check_extension_ui.py
python3 scripts/check_extension_ui_browser.py
python3 scripts/check_tui_interaction.py
python3 scripts/check_reasoning_tui.py
python3 scripts/check_images.py
python3 scripts/check_web_browser.py
python3 scripts/check_frontend_parity.py
python3 scripts/check_process_protocol.py
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**625 default workspace tests, 689 all-feature tests and 226 isolated core tests** pass.
Thirteen SWE-bench contract cases, four host-process cases and the binary-patch case prove
the meaningful behavioral boundaries:

- Issue, hint, hidden-test, unknown grading/image metadata, execution pin, model/config and
  concurrency changes cannot silently reuse an old generation. Changed requests reject before
  another factory call; existing artifacts remain intact. Unpinned SDK runs cannot resume.
- Concurrent leases prevent duplicate admission. Terminal failures are reused as failures.
  Missing/unsupported receipts, corrupt evidence or a started marker without a receipt never
  trigger automatic agent reexecution. --no-resume refuses an existing output.
- Every selected case has a prediction row. One deliberate setup failure remains empty and
  counts in the total; aggregate unknown usage stays unknown. Re-fingerprinting a forged
  prediction table cannot hide a mismatch with the captured patch.
- Full issue/future grading fields are preserved as hashes without reaching model input.
  Unknown/duplicate IDs and path-like case/job labels fail before generation or grading.
- Shared descriptor-scoped storage rejects symlink and special-file references. A FIFO with
  no writer fails promptly. Raw binary output is retained even below the preview limit.
  A Git binary patch larger than 64 KiB survives exact reverse/forward application.
- Host native argv preserves literal arguments. Deadline, cancellation and dropped-future
  cleanup own, signal and reap the same process group, without depending on an async cleanup task.
  Original Bash arguments/approvals and callback behavior remain covered by runtime tests.
- Pinned harness identity rejects incompatible/modified package contents before container
  launch. Historical complete grading can be verified/reused without Python or Docker.
  Changed rows/predictions/scope or incomplete prior publication cannot relaunch the harness.
- Official aggregate counts, ID sets, selected scope, raw capture and normalized ratio are
  checked. Total-instance resolved rate differs deliberately from submitted-only diagnostics.
  A subset cannot masquerade as the whole generation experiment.

Regular CI uses the fake Python harness fixture and no installed swebench/Docker for these
contract checks. It preserves offline evidence; hosted CI execution/upload is configured,
not claimed by local verification. The optional manual workflow exercises the actual package.

## Actual official Python/Docker evidence

The [interop record](baselines/s7-swe-interop.json) retains package/interpreter/image/input
identities and observed outcomes. The optional package was installed into a separate temporary
virtual environment; no global package, credential or production-agent dependency changed.

- Published PyPI wheel: swebench 5.0.2,
  SHA-256 `b7f0416a1e686eca22c2f749b5f816685a202835032f6683080e2b53545bbb62`.
- All 65 swebench/ package files aggregate to
  `b658165e24eb8c4a27d12586a68593ff27c4f8b3794a41991870b2000f726cc5`.
  The installed CLI help was invoked, rather than inferring release arguments from mutable
  upstream main. The release still supports --report_dir.
- Actual Docker grading checked one correct patch, one wrong patch and one empty prediction:
  selected/submitted 3, completed 2, resolved 1, unresolved 1, empty 1, errors 0. Overall
  fixture rate is **1/3**, with zero unstopped containers. The linked generation fixture made
  exactly three factory admissions, reused all terminal receipts, and retained its setup failure.
- A separate actual cancellation test waited until the owned container was running, sent
  SIGINT, observed exit 130, one owned container removed, none remaining and no score published.
  Cleanup uses only current fresh invocation authority, not a restored history token.

The Docker fixture uses a local immutable image ID and real file assertions emitting the
pinned parser's supported status format. No public SWE-bench task dataset, real model service
or gold model solution was requested. The 1/3 rate is a protocol fixture, not a SWE-bench
coding-success result. Existing user containers/images were not targeted or removed.

## Footprint and remaining scope

The [default footprint report](baselines/s7-swe-report.json) retains final-source samples,
comparison and limitations. Host process code reuses runtime's existing owner; no duplicate
agent loop or default tool is installed. Direct eval dependencies reuse locked packages;
optional Python/Docker tooling remains outside production Rust builds. Embedded Python probe/
cleanup source now participates in the source fingerprint.

The three final samples share Rust/manifests/embedded-assets SHA-256
`8b8c2795b48bafd29018820969af469f31eb9d017fa5e4a8ed8cfcf5d1fd5a88`.
The recorded revision is the parent because collection precedes commit. Resolved package
inventory remains 272; no locked package/version changed.

| Default metric | Observation | Change from coding archives |
| --- | --- | --- |
| Release CLI | 17.344 MiB | +0.008% |
| Release TUI | 17.305 MiB | -0.029% |
| Release SWE-bench | 17.240 MiB | +9.65% |
| 125-case smoke wall time, median | 387.76 ms | -2.11% |
| Smoke peak RSS, median | 7,808 KiB | -1.61% |
| Idle offline runtime RSS, median | 4,248 KiB | -0.38% |

All wall samples are retained: 387.76 / 401.10 / 383.70 ms. The evaluation-binary increase
includes schema-4 plan/receipt validation, lossless Git capture and official grading/ownership
logic; ordinary application binaries remain effectively unchanged. These observations remain
within the initial review thresholds, with the SWE-bench size increase close to its 10% review
threshold and therefore kept explicit. Existing targets/caches were reused; there is no clean
build, allocator or cache-isolation measurement and no optimization claim from small differences.

Actual service smoke/task scores, the identical-budget pinned Pi run, full feature matrix and
release audit remain open. Package/source/input hashes establish data consistency under a
trusted host, not publisher authenticity or an OS sandbox for arbitrary agent Bash/native code.
Image tags and third-party dependency state can be mutable; the recorded fixture pins its image
ID explicitly. Cleanup reports bounded observed quiescence, not a guarantee against arbitrarily
late Docker-daemon requests. No incomplete cleanup or capture becomes a successful score.
