# Local session exports and explicit publication (S5)

`export-session` produces a bounded JSON or self-contained HTML document containing the
complete canonical tree of one workspace-scoped session. It exports recorded entries,
parent links, selected leaf/revision, tool results, summaries, navigation and interruption
records. It does not export a repaired/provider-projected model transcript, restore grants,
run a model/tool, migrate storage or silently omit older branches.
Explicit [native context](boundary-hooks.md) retains its extension provenance and inert HTML
preview; the JSON form uses the compatible reserved Extension wire representation.

```sh
aicoder --workspace . export-session SESSION_ID --format json --output session.json
aicoder --workspace . export-session SESSION_ID --format html --output session.html
```

The destination's parent must exist. Publication creates a new private file atomically;
existing files/symlinks are never overwritten. Destinations use UTF-8 paths. An incomplete preparation, cancellation,
quota/serialization failure or stale source cannot publish a partial document. A failure
after destination visibility reports that fact so callers inspect before retrying.

## Source and bounds

Runtime takes an idle source lease and a stable metadata lock. It validates the canonical
workspace and archive before rendering, reads v3 records through the existing bounded
archive index, and verifies the source again before accepting the result. It does not load
all cold payloads or scan/fingerprint the whole archive for every page. Small legacy sessions
use their bounded compatibility snapshot; larger legacy input needs explicit migration.
Malformed/torn transactions retain the existing repository validation semantics.

Default limits are 128 MiB source bytes, 10,000 canonical entries and 16 MiB encoded output;
trusted callers may choose lower limits or raise them up to 1 GiB source, 100,000 entries
and 64 MiB output. CLI flags are --max-source-bytes, --max-entries and --max-bytes.
Archive record/index limits remain effective. Rendered bytes are immutable owned preparation,
not a serialized backend checkpoint. Storage staging is one bounded private temporary file;
its incomplete bytes are removed on failure/cancellation. Explicit output paths do not create
an implicit unbounded runtime export cache. Hosts control how many prepared objects they retain.
Metadata admission is nonblocking and rejects busy sources. POSIX file allocation reserves
the known bounded length where supported; ordinary write errors still abort before visibility.
Cancellation observed before the publication boundary prevents visibility; once a complete file is
linked, finishing its durability checks takes precedence over a late cancellation.

Schema v1 names `canonical_entries_complete`, `source_tail_complete`, and
`ignored_tail_bytes` separately. A torn final storage write is not canonical history; its
uncertainty is explicit, including unfinished turns. No synthetic terminal/tool record is
added. Storage checksum/transaction framing is not copied: this is an interchange snapshot,
not a byte-for-byte JSONL backup or an authorization-bearing restore file. Local JSON and
HTML include private recorded text/paths and any opaque/image data already present in history.
External output-artifact references are preserved as recorded; artifact bytes are not bundled.

## HTML

HTML has no scripts, external assets, remote images or active model/tool markup. Every
untrusted string is escaped into text; URLs/provider data are displayed as recorded data,
not fetched or turned into executable links. A strict CSP disables script/connect/frame/
object/form execution. Each record is shown with identity, parent and selected-branch state;
human-readable previews are limited to 8 KiB per block and label clipping; complete raw
recorded payloads remain inspectable in native disclosure sections. The page explicitly distinguishes raw history,
summary/navigation metadata and unfinished turns. No pricing or outcome is invented.

## Explicit publication adapter

Local export never invokes a publisher. `runtime::exports::ExportPublisher` accepts the
same immutable PreparedExport only when its host explicitly calls it. The concrete
DirectoryPublisher publishes a new complete file to a host-selected directory; an optional
trusted public base URL produces a receipt URL. It does not start/deploy a server, upload to
an implicit third party, test remote hosting or grant anyone filesystem authority.

```sh
aicoder --workspace . share-session SESSION_ID --directory /trusted/share \
  --base-url https://example.invalid/agent-exports/
```

This is an explicit publication action and defaults to HTML. Adapters for other destinations
can implement the same typed interface without changing the agent loop. Network publication
and credential acquisition belong to the supplied adapter, outside core and recorded history.
The default directory adapter performs no network access. Publication errors retain whether
a destination may already be visible; completed operations are never retried automatically.

The [JSON schema](protocol/session-export-v1.schema.json) and
[recorded offline fixture](protocol/session-export-v1.json) define the interchange shape.
`python3 scripts/check_export_schema.py` checks the fixture without remote schema access.
The non-cryptographic fnv1a64 receipt fingerprint identifies content; it is not a signature
or authorization token. Changes after successful preparation do not rewrite that snapshot.

Both operations have structured receipts, reuse source/serialization budgets, and need no
model credential. CLI logs/receipts do not dump exported content. See
[acceptance and measurements](s5-exports-acceptance.md) for evidence and limits.

An offline example exercises both formats and explicit publication:

```sh
cargo run --locked -p aicoder-runtime --example offline_exports
# Optional retained demo files and real-browser check (new output directory):
cargo run --locked -p aicoder-runtime --example offline_exports -- artifacts/export-demo
python3 scripts/check_export_browser.py artifacts/export-demo/session.html artifacts/export-demo/session.json
```

Browser verification uses the optional Playwright development requirements shared with the
Web example. Normal export and publication binaries require no browser, Python or JS runtime.
