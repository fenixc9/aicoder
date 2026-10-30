# S5 canonical export and publication acceptance

Runtime now prepares complete canonical JSON/HTML exports and exposes an explicit typed
publication boundary. CLI export-session writes a new private file; share-session invokes
the concrete directory publisher. This completes S5's local export/publication item.
TUI improvements and image input remain open, so S5 and full Pi parity remain incomplete.

## Contract

[Session exports](session-exports.md) define schema v1, all-branch scope, bounded source/
entry/output admission, native HTML previews and unchanged canonical records. Preparation
pins an idle source under nonblocking metadata/run admission, validates existing repository
checksums/structure and visits records through one bounded index. It reads pinned descriptors,
not provider projections or private index placeholders, and verifies file identity, timestamps,
length and fingerprint before returning immutable owned output. Legacy compatibility remains
bounded and does not migrate storage. Source and output bytes are separate budgets.

The JSON manifest distinguishes canonical completeness, torn storage tails and unfinished
turns. No synthetic repair/tool/terminal entries are added. Artifact references remain data;
external bytes are not bundled. HTML previews can clip at 8 KiB per block, explicitly labelled;
complete raw records remain in native disclosure sections. Model/tool markup and URLs are
text, with no scripts, remote images, links, fetches or JS runtime. The schema is an
interchange contract, not a JSONL backup, backend checkpoint or restored authorization.

Prepared bytes are immutable and fingerprinted with explicitly non-cryptographic fnv1a64.
Publishing an older prepared revision does not silently include later history. The publisher
is invoked only by a host's explicit call/share command. Directory publication pins its parent,
stages bounded private bytes, syncs them, links atomically without replacing an existing file,
and syncs/checks the directory. POSIX allocation reserves known length where available.
Post-visibility errors expose CommitFailure.visible so callers inspect instead of retrying.
CLI cancellation awaits owned IO; checks are cooperative around synchronous filesystem work.

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
cargo run --locked -p aicoder-runtime --example offline_exports
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p aicoder-web --features server --no-deps
python3 scripts/check_event_schema.py
python3 scripts/check_rpc_schema.py
python3 scripts/check_export_schema.py
```

**477 default workspace tests**, **487 all-feature tests** and **187 isolated core tests**
pass. Eleven focused additions (seven runtime integrations, two filesystem/source unit
cases, two actual CLI integrations) extend the prior 466/476 inventory:

- JSON round-trips exact recorded entries and all parent branches through Session::from_entries;
  selected ancestry remains explicit and unfinished calls are not repaired. No run token is restored.
- Cold archive payloads export even when neither full snapshots nor a model window can load;
  descriptor placeholders never appear in exported data.
- HTML escapes hostile tags/quotes/URLs. UTF-8 preview clipping preserves the complete raw
  canonical payload and never creates executable markup or replaces record data.
- Source/output/entry budgets, wrong workspace, active runs, busy metadata admission and
  cancellation fail without changing canonical history.
- Torn tails remain byte-for-byte untouched and are declared separately from complete canonical
  entries; small legacy export preserves version/IDs without migration or backup writes.
- Owned snapshots publish exact reviewed bytes even after later history is appended. Existing
  files and symlinks are preserved, output is private, cancelled publication creates no file,
  invalid credential-bearing URLs fail, and temporary files are cleaned up.
- A UTF-8 alias resolving into a non-UTF-8 directory fails before output visibility/receipt
  serialization. Destination identity is checked after canonicalization.
- An external writer ignoring the source lease causes final validation to reject preparation.
  Moving a destination after atomic visibility produces an explicit visible=true failure;
  the complete file remains and no retry or partial stage is produced.
- Real CLI processes export and share with no model credential, report only metadata on stdout,
  preserve recorded source bytes, reject foreign workspaces/limits and never replace old output.

The [offline example](../crates/aicoder-runtime/examples/offline_exports.rs) executes both
formats and actual directory publication without a provider. The published
[JSON fixture](protocol/session-export-v1.json) was captured from it; only its workspace
path is anonymized. Six canonical entries pass the new schema checker, alongside the prior
319 event and 44 RPC frames. CI runs the example and all three schema checkers.

The optional [browser checker](../scripts/check_export_browser.py), using Playwright 1.62.0 /
Chromium 151.0.7922.34, verifies six rendered records, matching selected-branch flags, literal
hostile HTML, absence of script/image/frame/object/form DOM nodes, and zero external requests.
A screenshot was reviewed; generated screenshots and actual session/temp files are not committed.
The checker uses intercepted local HTML; no public service, real credential or upload is used.

## Scope and cost

No new dependency or package version is introduced. Public core/event/session formats stay
unchanged; runtime owns the concrete export IO/rendering and CLI only adapts host commands.
No model, terminal, HTTP server, browser, Python or JS runtime is required for export/publication.
Optional browser development checks reuse the Web example's existing requirements.

The default publisher copies to an explicitly configured local directory. Its URL receipt
is supplied-host mapping, not proof of remote hosting or an implicit service deployment.
Other publishers implement the typed capability and own their credentials/cancellation.
Explicit exported history can contain private recorded paths/text/provider/image data; there
is no silent redaction that changes canonical facts. Memory-only repositories are not file
sources for this concrete runtime exporter. Larger legacy storage needs explicit migration.

OS/filesystem sync, cleanup and syscall behavior remains platform-dependent; cancellation is
cooperative, and adapters can already have made complete bytes visible when a late failure
occurs. No automatic retry, rollback of published data, remote verification or success-rate
claim is introduced. Prepared objects are bounded individually; embedding hosts bound their
own retention/concurrency. The directory adapter does not maintain an implicit export cache.

## Measurements

[Three final-source default samples and three export-case samples](baselines/s5-exports-report.json)
use the pinned toolchain and the same default scripted runtime workload as
[S5 Web](baselines/s5-web-report.json). Source SHA-256 is
`03b59462977acc963a55994f3162bc289ff8f741e143442345ad216992b078df` and includes embedded
HTML/JS/CSS. Revisions name the parent because measurements precede the feature commit;
the digest identifies the measured sources/manifests/assets.

| Metric | Final measurement | Change from S5 Web |
| --- | --- | --- |
| Resolved default packages | 250 | unchanged |
| Release CLI binary | 15.149 MiB | +1.68% |
| Release TUI / SWE-bench | 14.445 / 13.519 MiB | -0.05% / -0.04% |
| Offline smoke wall time, median | 390.84 ms | +1.48% |
| Offline smoke peak RSS, median | 7,424 KiB | +1.75% |
| Idle runtime RSS, median | 4,116 KiB | -0.68% |

All 25-iteration wall samples are retained: 389.53 / 397.56 / 390.84 ms. Peak RSS is
7,296 / 7,424 / 7,552 KiB; idle RSS is 4,228 / 4,040 / 4,116 KiB. These local process/time
wrapper measurements vary; they establish neither a performance improvement nor a proven
latency regression. Target directories/page caches are reused, not clean/cold builds.
The archive scan helper is shared with existing binaries, so measured byte differences
are recorded rather than assumed zero. No new dependency is required by the feature.

The actual release offline_exports case renders both formats and publishes immutable HTML
to a directory: six entries across two branches, 2,261 JSON bytes and 7,229 HTML bytes.
Its three process/GNU-time wall samples are 50.46 / 23.55 / 21.69 ms (median 23.55 ms),
with 4,224 / 4,352 / 4,096 KiB peak RSS (median 4,224 KiB). The example verifies published
bytes match preparation. These include temporary-directory creation, allocation and sync;
they do not measure cold caches, large-archive throughput, browser memory, provider latency
or real coding success. The large/cold archive path is tested independently above.
