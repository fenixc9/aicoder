# Bounded read-only coding tools

Runtime's default registry now supplies `list_directory`, `glob`, `grep` and
`read_file` to the same CLI, TUI and evaluation assembly. These tools are `ReadOnly`;
write/edit/Bash approval is unchanged. Applications can register the public
`ListDirectoryTool`, `GlobTool`, `GrepTool` and `ReadFileTool` individually.

## Discovery and matching

| Tool | Input | Result |
| --- | --- | --- |
| `list_directory` | `path` (default `.`), `limit` (1–1000, default 100) | Immediate entries with workspace-relative `path` and `kind`: file, directory, symlink or other |
| `glob` | Required `pattern`, optional `path`, `limit` | Regular files in ascending workspace-relative path order |
| `grep` | Required `pattern`, optional `path`, `glob`, `max_results` (1–1000, default 100), `literal`, `case_insensitive` | Ascending path/line records and a display `matches` string |

All three accept `hidden` (default false), `respect_gitignore` (default true) and
`cursor`. Explicit paths must resolve inside the canonical workspace. Recursive
search never follows symlinks; listing reports their type. An explicitly requested
symlink target is permitted only if canonical resolution stays inside the workspace.
Implicit traversal omits `.git` even with `hidden: true`. Special files are not searched;
`read_file` checks regular-file identity and opens nonblocking on Unix so FIFOs cannot
wait for a writer. Non-UTF-8 path names produce an explicit `unsupported_path` error.

Glob syntax comes from [globset](https://docs.rs/globset/0.4.20/globset/): `*`, `?`,
classes, alternation and `**` are supported. Patterns with `/` are relative to the
selected scope; `src/*.rs` matches one directory level. Basename patterns such as
`*.rs` match at any depth. Set `hidden: true` to search dot paths. Project `.gitignore`
files use the [ignore crate's parser](https://docs.rs/ignore/0.4.33/ignore/gitignore/),
with root-to-scope inheritance, nested precedence, negation and directory pruning.
This does not read user-global ignore configuration, parent-workspace rules, `.ignore`
or `.git/info/exclude`. `respect_gitignore: false` disables project ignore loading.
Explicit file scopes are searched even if otherwise hidden/ignored. Ignore rules are
filters, not access controls.

`grep` uses bounded [Rust regex](https://docs.rs/regex/1.13.1/regex/) matching per line;
`literal: true` escapes the pattern and `case_insensitive: true` enables Unicode case
folding. This replaces the old collect-all ripgrep subprocess. Binary detection (NUL
or invalid UTF-8) skips the rest of that file and increments `binary_files_skipped`;
matching lines already observed remain valid results. Grep does not implement multiline
or PCRE-only expressions. A matching line appears once, with its first `match_start`.

Each record has `path`, one-based `line`, absolute file `byte_offset`, a bounded
`text` excerpt, its `text_offset` within the line, `match_start`, `text_truncated`
and a `read_cursor`. Excerpts center near the first match and can omit parts of long
lines. Use `read_file` with the record's `path` and `read_cursor` to inspect the full
line through bounded pages. Structured records are authoritative when filenames or
content contain newlines; `matches` is for display and existing consumers.

## Continuation contract

A successful page contains `has_more` and nullable `next_cursor`. Copy the returned
cursor unchanged into the same tool/query, retaining its path/filter settings; page
size may change. `has_more` means input remains to scan, not that another match is
promised. A grep page may be empty when its byte-scan budget is exhausted. Continue
until `has_more: false`; never interpret an empty page alone as end-of-search.

```json
{"pattern":"TODO","glob":"*.rs","max_results":20}
```

On subsequent calls, add `"cursor": "<exact next_cursor>"`. Cursors are opaque,
versioned JSON strings scoped to the canonical query. They are not authorization
capabilities or signed tokens. Unknown versions, oversized/malformed tokens and query
mix-ups fail with `invalid_cursor`. File cursors retain length, modification timestamp
and, on Unix, device/inode identity. Changed/missing continuation files fail with
`stale_cursor`; restart the affected query. These checks detect ordinary changes and
atomic replacements, not deliberate same-inode rewrites that restore timestamps.

Directory pages enumerate and sort the bounded live scope again. They resume strictly
after the prior path; no directory snapshot is retained. Concurrent additions before
that path can be missed, and filters/rules may change between pages. Use a stable
workspace or restart discovery after edits when a complete listing matters. File
page stability does not establish a transaction across an entire tree.

## File reads and long lines

`read_file` accepts `path`, `offset_line` (one-based, default 1), `limit_lines`
(1–4000, default 400), or `cursor`. Do not combine a cursor with `offset_line`.
It reads a fixed-size byte window, never an unbounded line/string. Pages preserve UTF-8
boundaries; incomplete/invalid UTF-8 at EOF is an error. Text is literal except that
a terminal LF and its optional preceding CR are removed from each returned page.
Interior line endings remain as stored. This is a text inspection API, not a raw-byte
archive/export API.

Pages also carry a `revision` for optional [mutation preconditions](file-mutations.md).
It is a stat/identity fingerprint, not a content hash.

`lines_read`, `end_line` and `end_line_complete` describe the returned fragment.
If output ends inside a line, `content_truncated` is true and `next_offset_line`
remains on that same line. `next_offset_byte` identifies the exact continuation;
`next_cursor` carries both byte and line positions. Copy the cursor to avoid re-reading
or skipping the rest of a wide line. The legacy practice of always incrementing
`next_offset_line` after truncation is no longer valid.

## Bounds, errors and cancellation

Successful **complete JSON output objects**, including escaped text, metadata and
continuation tokens, fit `min(ToolContext.max_output_bytes(), 64 KiB)`. They therefore
survive core dispatch's generic clipping as structured values. Limits below 512 bytes
fail early; budgets that cannot fit a single record/token return
`output_limit_too_small`, rather than an unusable partial JSON string. Global run-output
budget exhaustion remains a terminal run stop and can supersede tool paging.

| Resource | Bound / behavior |
| --- | --- |
| Read page buffer | At most output cap + 4 bytes, plus an 8 KiB buffered reader |
| Initial read `offset_line` scan | At most 16 MiB consumed before an explicit `scan_limit`; buffered read-ahead is bounded |
| Grep scan per call | 16 MiB accounted input plus at most one 8 KiB buffer of read-ahead; cursor resumes at a complete line boundary |
| Grep line buffer | 64 KiB including its newline; larger lines return `line_too_long` with path/line and a recovery `read_cursor` |
| Grep excerpt / regex | 1024-byte excerpt; 4096-byte input pattern; 1 MiB compiled-regex and DFA cache limits |
| Discovery scope | 20,000 visited entries, 4 MiB cumulative path text, depth 64 below scope, 4096-byte input/result paths |
| Ignore sources | 64 KiB each, 1 MiB total source text and 4096 lines per query; symlink/non-regular ignore files rejected |
| Pages / tokens | Up to 1000 entries or matches, 16 KiB token input; actual output also obeys host cap |

Scope/depth/ignore bounds return `scope_limit` without presenting an incomplete tree
as complete; narrow `path` or explicitly disable ignore loading when appropriate.
A distant `offset_line` can return `scan_limit`; continue using read cursors, including
those returned by grep. Invalid patterns have `invalid_glob`/`invalid_regex` errors.
IO/permission failures stop the query instead of silently disappearing from results.

Each tool applies the context timeout when called directly; core dispatch additionally
applies run cancellation, tool timeout and global budgets. Async read/enumeration loops
yield during IO and retain bounded buffers. Filesystem latency and synchronous pattern
compilation are not forcibly preemptible. Canonical path checks and no-follow file
opens do not provide a descriptor-based sandbox against hostile concurrent directory
replacement. Bash remains outside this read-only contract.

Run the complete offline discovery/search/read workflow with no model service:

```sh
cargo run --locked -p aicoder-runtime --example offline_file_tools
```

See [acceptance evidence](s3-file-tools-acceptance.md), [migration](migrations.md)
and [the remaining S3 work](roadmap.md).
