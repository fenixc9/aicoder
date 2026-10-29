# Project instructions, skills and prompt templates

The shared runtime discovers bounded, inspectable resource generations. CLI, TUI and
SWE-bench use the same loader and core system-prefix interface. These are instruction
and text-resource facilities; they do not execute extensions/scripts, grant tool
permissions or implement a sandbox.

## Discovery and scope

| Resource | Global user location | Parent/workspace locations |
| --- | --- | --- |
| Project instructions | state_root/AGENTS.md | Each directory's AGENTS.md; nested workspace directories are scanned |
| Base replacement | state_root/SYSTEM.md | SYSTEM.md, then .aicoder/SYSTEM.md in each ancestor/workspace directory |
| Appended system text | state_root/APPEND_SYSTEM.md | APPEND_SYSTEM.md and .aicoder/APPEND_SYSTEM.md |
| Skill packages | state_root/skills/**/SKILL.md | .aicoder/skills/**/SKILL.md |
| Prompt templates | state_root/prompts/**/*.md | .aicoder/prompts/**/*.md |

state_root retains [the runtime configuration rules](runtime-configuration.md).
Global resources are considered first, then filesystem ancestors from root to the
workspace, then nested AGENTS.md in deterministic path order. Parents contribute only
fixed resource names/collections; the loader never recursively walks parent projects.
Nested discovery reads AGENTS.md only. SYSTEM/skill/prompt discovery uses the declared
roots rather than arbitrary nested .aicoder folders. A directory containing SKILL.md
is a package boundary: scripts, references and large asset trees are not enumerated
at startup.

All discovered applicable AGENTS text is available **before the first model call or
mutation**, labeled with its source and applicable directory. Global rules apply to
the whole workspace; nested rules apply to that directory/descendants, with nearer
rules appearing later. This supplies scoped guidance, not a kernel enforcement of
natural-language rules. No late discovery happens after an edit has already occurred.

The nearest SYSTEM file wins; .aicoder/SYSTEM.md wins over the plain file at the same
level. Configured system_prompt has higher priority when its configuration rank is
higher or equal: explicit > environment > project > user > default. Ancestor/workspace
SYSTEM files have project rank; global SYSTEM has user rank. Null/empty base overrides
retain their configured meaning. APPEND_SYSTEM and AGENTS text are separate additions;
clearing the base does not disable project rules. To disable those, use the switches.
Custom host system messages are preserved. Benchmark task instructions following the
known shared base are preserved when a file replaces that base.

Canonical resource identity is deduplicated per kind. Global discovery wins when the
same physical file is also found by an ancestor/project path. Disabling global resources
prevents those global locations from reentering through a matching ancestor .aicoder
root. Name collisions across roots use the nearer skill/template; duplicate names
within one root are errors. The manifest retains shadowed entries with active=false.
No resource bodies are printed by the source inspector.

Default nested/collection exclusions are .git, .hg, .svn, target, node_modules, .venv,
.aicoder, artifacts, dist, build, __pycache__, .cache and .tox. This is a deterministic
name policy, not a full .gitignore interpreter. Nested directory symlinks are not
traversed. Excluded trees have no automatically discovered nested rules; change the
policy explicitly if editing them requires those rules.

## Skills and assets

A skill package contains a UTF-8 SKILL.md:

```markdown
---
name: rust-review
description: >-
  Inspect Rust changes and choose relevant tests.
---
Read the code before suggesting a change. Supporting notes are in references/.
Use scripts/check.sh only when appropriate and permitted by the host.
```

Names must match the containing directory and use 1–64 lowercase ASCII letters,
digits/hyphens, without leading/trailing/consecutive hyphens. A nonempty description
of at most 1024 characters is required. YAML is parsed with yaml-rust2, using its event
API and bounded UTF-8 frontmatter; quoted/multiline text and nested metadata work.
Aliases, multiple documents, excessive depth/nodes and non-scalar name/description are
rejected instead of expanding untrusted structures. The frontmatter limit is 8 KiB,
16 levels and 2048 parser events. Unknown metadata never changes permissions.

Initial context includes only available names, descriptions and source paths, not all
skill bodies. `load_skill` returns a relevant skill's instructions and asset directory.
Its main-file fingerprint must still match discovery; otherwise activation fails and
requires reload. `disable-model-invocation: true` omits a skill from automatic listing
and rejects autonomous body loading. Explicit `/skill:NAME request` still works. This
is an invocation preference, not an access/security boundary or session-wide tool grant.

`read_skill_file` reads UTF-8 pages from a known package, including references outside
the coding workspace but inside that approved package. It returns offsets, has_more,
source and a page fingerprint. Pages are 1–32768 bytes and returned offsets remain UTF-8
boundaries. Relative paths are limited to 4096 encoded bytes / 128 components, and
encoded page metadata must also fit the tool output capacity. Absolute/parent-traversing paths, links, devices/FIFOs and binary/non-UTF-8
pages fail. Scripts are never executed by discovery, activation or asset reading; model
shell use still follows the existing approval policy. Manual-only main bodies remain
explicit-invocation-only, while package references are ordinary bounded resource reads.

On Unix, skill directory handles are pinned and relative components are opened with
openat/O_NOFOLLOW, so replacing a path cannot redirect reads through links outside the
package. Other-platform fallback behavior is not claimed as verified by Linux tests.
Ordinary instruction/template resource links are rejected, and collection symlinks are
not followed. The explicitly configured user root itself is canonicalized as a trusted
root. Resource references are not an arbitrary global-file read tool.

## Prompt templates

Templates are Markdown in a declared prompts root. The filename stem is the command
name, with the same name grammar. Optional YAML description is used for inspection.
exit/reload/resources/config are reserved names. Invocation begins at the first character:

```text
/review 'file with spaces.rs' tests
/skill:rust-review inspect this change
//literal-leading-slash
```

Quote/escape parsing uses shlex, not a shell. `$1`, `$2`, `${10}`, `$@`/`$ARGUMENTS`,
`${@:2}` and `${@:1:2}` expand positional/all/sliced arguments. Missing positional
arguments become empty; indices start at 1. `$$` is a literal dollar and unrelated
shell variables such as `$HOME` remain text. No variable/command execution occurs.
Expansion checks its output incrementally, preventing repeated placeholders from
multiplying a small invocation into unbounded output. Expanded user text includes its
resource source label; PreparedPrompt also returns the exact ResourceInfo/fingerprint.
Core never interprets untrusted historical/dataset user messages as commands: hosts
explicitly call Runtime::prepare_prompt for a new user invocation.

## Limits and switches

Resource discovery fails as a whole on invalid required files or limits; no partial
catalog is silently used. Defaults are 64 KiB/file, 256 KiB retained text/metadata,
4 MiB scanned text, 256 files, 20000 directory entries and depth 32. Skill bodies are
scanned/fingerprinted but not retained until activation; templates/instructions are
retained. Counts include shadowed candidates. Paths/manifest structures and frames
already handed to an application have their own overhead outside retained-text bytes.

Supported ceilings are 1 MiB/file, 8 MiB retained, 32 MiB scanned, 1024 files, 100000
entries and depth 64, with at most 128 excluded directory components. These bounds are
separate from model context/output/task quotas. Large immutable system context may
still exceed a configured context window; the engine reports that instead of silently
dropping project rules. A skill too large for the per-tool output capacity can be paged
with read_skill_file.

JSON settings use settings.resources with independent enabled/global/parents/nested/
instructions/system/skills/prompts booleans, the limits above and excluded_directories.
They follow [normal layered precedence](runtime-configuration.md). Environment switches
are AICODER_RESOURCES, AICODER_GLOBAL_CONTEXT, AICODER_PARENT_CONTEXT,
AICODER_NESTED_CONTEXT, AICODER_INSTRUCTIONS, AICODER_SYSTEM_FILES, AICODER_SKILLS and
AICODER_PROMPTS. CLI/TUI support --no-resources, --no-parent-context, --no-skills and
--no-templates; SWE-bench also supports --no-resources. JSON-config discovery and resource
discovery are separate switches: --no-config does not disable AGENTS/skills.

```sh
aicoder --no-parent-context resources
aicoder expand-prompt '/review source.rs'
aicoder --prompt '/skill:rust-review inspect the failure'
```

Configuration inspection remains lazy and does not read resource bodies. The explicit
resources command freezes/inspects the generation, and expand-prompt performs a dry
expansion without model credentials or session writes. Full registry/agent assembly
freezes resources on first access to that Runtime. Snapshot failures are cached until
reload so an existing generation cannot silently change after a failed discovery.

## Reload, core and benchmark boundaries

Instructions/templates remain their loaded text. Skill activation verifies its main
file against the snapshot. Asset pages are explicit current reads with their own page
fingerprints, not a frozen archive of every package file. FNV-1a fingerprints identify
ordinary changes; they are not cryptographic authentication or a tamper-proof archive.
No script execution/replay guarantee is implied.

Runtime::reload_resources builds and validates a new generation with the same settings;
Runtime::reload also rereads settings as previously documented. Existing Agent/tool
instances retain their old snapshot. TUI /reload swaps resources only while idle; a
failed reload leaves the old agent intact. /resources shows active source paths. Editing
settings or an explicit dotenv source has separate restart/full-reload semantics.
Rejected queued template input keeps the original invocation in the draft.

Core's InputContextProvider returns only a system_prefix and opaque provenance. It cannot
return a replacement user/tool history. The executor moves the existing history behind
that prefix, preserving mid-history recovery system markers and avoiding payload copies.
Preparation observes cancellation/execution deadlines. Provenance is checkpointed before
the first model call; failures stop before model work and retain prepared progress.
The bundled session journal stores an aicoder.input_context extension; system file bodies
are not fabricated as user messages. RunProgress and the optional terminal input_context
field carry metadata. The optional protocol-v1 field is additive; old frames remain readable.

Use executor_builder_for(provider, workspace) for an isolated case workspace, rather
than rebinding workspace after resource assembly. It disables ambient parent discovery
and reuses a frozen global generation. SWE-bench loads only base-checkout project resources,
retains benchmark-specific instructions, and records actual source metadata in progress/
trace plus the frozen global manifest in parameters. It never loads grading patch/answer
fields as context. Checkpoints include base_commit and input-context version; older or
mismatching inputs regenerate cases. Resource bodies/assets are not a general replay log.

```sh
cargo run --locked -p aicoder-runtime --example offline_resources
```

The offline fixture discovers project rules, expands a template, loads a skill through
the real loop, records provenance in a memory session and verifies that scripts were
not executed. Tests additionally cover bounds, links, precedence, reload, CLI request
bodies and case isolation. Concrete tool/storage extraction and remaining S3 tool/approval
work are still open.
