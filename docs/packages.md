# Versioned resource packages (S6)

Packages are explicit, content-pinned bundles installed under trusted host storage. A
versioned aicoder-package.json lists every payload file, resources, exact package/program
dependencies, declared permissions and optional process/MCP entries. Installation imports
bounded regular files into a private content-addressed object, then atomically publishes a
registry record. It never runs package scripts or installs a language runtime.

The registry keeps exact SHA-256 pins and old immutable objects. Activating a new revision,
disabling or uninstalling changes future selections; existing resource/agent snapshots keep
their captured generation. Uninstall revokes future trust, not canonical session history.
No automatic garbage collection removes objects needed by existing handles.

## Manifest and trust

Manifest version 1 contains id/version, a files allowlist, exact dependency pins, resource
entry paths and permissions. Instructions, skill main files, prompt templates and theme
assets are relative to the pinned root. Files cannot escape through absolute/parent paths,
links, devices or undeclared assets. Package resources use the existing bounded loader,
frontmatter/template rules and pinned skill-directory readers. Workspace resources retain
their precedence. Per-file integrity is checked before activation/asset pages.

Executable entries require declared execution permission and explicit host consent for
that exact content digest. A package chooses a bundled executable or a logical program
dependency, not an arbitrary host executable, credential source or storage path. Program
profiles are host-owned; their environment reference resolution stays outside manifests and
project selection. Peer capabilities still default conservatively and run through core.
Installing or inspecting a package never starts it. Optional activation uses the existing
process/MCP adapters and explicitly awaits cleanup; partial activation failure tears down
new owners without replacing a working snapshot.

Package dependency versions/digests are exact, bounded and cycle-checked. Missing or changed
dependencies fail resolution; no hidden downloader or lifecycle script grants authority.
Local directories are the implemented import source; remote Git/npm acquisition is deferred
until it has bounded download, extraction and lifecycle rules. Pin identity depends on bytes
and executable modes, rather than an upstream branch/tag name. User-facing activation and source inspection report dependency/
permission metadata, without resource bodies or credential values.

## Runtime and sessions

Runtime freezes selected package pins with its resource generation. Explicit reload builds
and validates a replacement first; errors preserve the previous runtime/agent. Evaluation
uses frozen globals and base-checkout resources, never grading fixtures as package inputs.
Project settings may select trusted logical packages; explicit host locks select exact pins, but cannot redefine host launch
profiles or private package roots.

Session state records exact selection and declared bounded public domain state. Restore
revalidates pins against host trust and field declarations; missing/uninstalled/untrusted
content cannot be installed or executed from history. Backend handles, remote plan/MCP
continuations, credentials and tool grants are not state fields or restored authority.
Canonical user/tool history and branch provenance remain intact.

Native Rust registration, process transport, MCP and frontend rendering keep their existing
boundaries. No TypeScript plugin ABI or mandatory JS/Python engine is introduced. The host
owns the decision to activate code and adopt a new snapshot at an idle boundary.

## Local workflow

A minimal `aicoder-package.json`:

```json
{
  "schema_version": 1,
  "id": "review",
  "version": "1.0.0",
  "files": ["rules.md", "prompts/review.md"],
  "resources": {"instructions": ["rules.md"], "prompts": ["prompts/review.md"]},
  "permissions": {"instructions": true}
}
```

```sh
aicoder packages inspect ./review-package
aicoder packages install ./review-package
aicoder packages list
aicoder packages lock > packages.lock.json
aicoder --package-lock packages.lock.json resources
aicoder packages disable review
aicoder packages enable review
aicoder packages select review 1.0.0 <sha256>
aicoder packages remove review
```

Commands emit JSON without resolving model credentials. `--disabled` installs without an
enabled root; `--allow-execution` explicitly trusts executable declarations for the imported
pin, but does not launch them or approve their tool calls. `--no-packages` disables package
selection. `--no-resources` disables resource discovery. TUI's existing `/reload` adopts text
resource updates after full validation; failed reload leaves its current executor intact.

Settings accept `packages.enabled` and `packages.selected` (logical ID array), using existing
explicit/environment/project/user/default precedence. `AICODER_PACKAGES_ENABLED` is boolean;
`AICODER_PACKAGES` is a JSON ID array (maximum 8 KiB, 32 unique names). Disabling global
context hides default globally enabled packages; an explicit ID selection still works.
Private storage is fixed at `<host state_root>/packages`; project settings cannot replace it.
The effective resource manifest reports exact package pins with the frozen generation.

## SDK lifecycle

`PackageStore::install_local`, `enable`, `select_revision`, `uninstall`, `snapshot` and
`Runtime::package_snapshot` expose the same lifecycle to CLI/Web/TUI/bench hosts. Resource
order is global, dependency-first packages, parent/workspace resources, nested instructions.
Two packages declaring the same skill/template name fail reload; workspace definitions can
override them. Instructions append to the protected runtime prefix and never replace canonical
user/tool records. Skill assets are allowlisted and SHA-256 verified even when read in pages.
Themes are validated JSON assets accessible via `PackageObject::read_asset`. The explicit
`Runtime::package_theme_asset` API applies host policy, current installed trust, declared
theme kind and caller byte limits, returning immutable data/pin metadata without code
activation or creating absent storage. TUI [package selectors](tui-interaction.md) interpret
these bytes using its existing palette format; colors remain outside runtime/core types.

`PackageSessionState::capture/save/restore` stores exact pins and declared primitive public
fields on the selected session branch. Saves require the expected revision and idle run
lease; restores read a real bounded extension record, including from a cold v3 archive,
and revalidate installed trust. `Runtime::with_package_state` builds and validates a replacement;
`--package-lock` uses the same path. Stock frontends do not automatically restore executable
process state from history. Runtime resource reload explicitly reselects current host policy;
applications wanting historical pins pass the restored state instead. See the offline
`offline_packages` example for a full lifecycle with no model or scripts.

With `process-extensions`, `PackagePrograms` maps logical names to host-owned absolute
executables, exact host-attested versions and environment references. `PackageSnapshot::activate`
preflights every entry, starts existing process/MCP adapters, and returns a `PackageActivation`.
Pass its `extensions()` to the existing executor builder; retain the activation owner and await
`shutdown()` at the idle lifecycle boundary. The `mcp-stdio` feature is needed for MCP entries.
Partial activation failure awaits all new owners. Existing executors remain the host's choice
until a successful replacement is installed. Stock CLI/RPC/TUI/Web use the explicit
[--activate-packages host path](host-extensions.md); installation/resource reload still do
not start package code. The optional --package-programs file supplies host-owned dependency
profiles. Direct/package owners preflight together and publish one complete catalog.
Dropping a PackageActivation invalidates retained tools and signals its owned processes;
await shutdown to confirm cleanup. Catalog versions retain the package content digest even
when resource reload selects a newer text generation. This digest identifies package bytes,
not an automatic identity proof for host-attested external program dependencies.

Bundled executables must be allowlisted regular files with executable mode. Dependency entries
must name a declared program requirement. Arguments are literal strings with only `{package}`
and `{workspace}` substitution; no shell expansion or inherited environment is used. Version
strings are exact opaque labels, not ranges or automatic binary-version verification. These
are trusted programs running with the user's permissions; this is not a process sandbox.

## Bounds and storage

Defaults: 64 KiB manifest, 1,024 payload files, 16 MiB per payload, 64 MiB per package,
256 retained objects and 512 MiB retained payload. Trusted overrides have hard ceilings of
32 MiB/file, 128 MiB/package, 512 objects and 4 GiB/store. Dependency closure/selection has
32 packages, 32 executable entries and bounded path/depth scans. Registry JSON is at most
1 MiB; public session/lock state is at most 32 KiB, with at most 64 primitive fields/package
and 4 KiB/string. Exact source bytes, relative paths and executable flags determine SHA-256.

Imports stage only declared regular files; `.env` paths, links and special files fail.
Cancellation/validation failure leaves the previous registry authoritative. Publication uses
an exclusive registry lock, synchronized staging and atomic rename; post-visibility durability
errors require inspection before retry. Uninstall revokes trust but retains object bytes for
existing generations; storage budgets include retained/orphan objects. There is no automatic
GC, dependency downloader, lifecycle hook or script engine. A sealed asset page verifies its
whole bounded file before slicing, so its scan memory can reach the per-file ceiling even
when the returned page is small. Public state is caller-supplied domain data, never a safe
place for secrets. Installed content can be modified by its OS owner; integrity checks detect
changes before use, but hostile same-user processes are outside this host-storage boundary.
