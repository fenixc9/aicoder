# Core contracts and runtime backends

Core owns the model-neutral execution loop, tool dispatch/approval contracts,
completion policies, session domain and `MemorySessionRepository`. Runtime owns
filesystem/process implementations and application assembly. No extra crate,
service, database or dependency package is introduced by this extraction.

| Capability | Public API |
| --- | --- |
| Custom tool, registry, dispatcher and approval | `aicoder_core::tools` |
| Default coding registry | `aicoder_runtime::tools::default_registry` |
| Read/write/edit/search/Bash | `aicoder_runtime::tools::{ReadFileTool, WriteFileTool, EditFileTool, GrepTool, BashTool}` |
| Directory and glob pages | `aicoder_runtime::tools::{ListDirectoryTool, GlobTool}` |
| Canonical tool path checks | `aicoder_runtime::tools::WorkspacePaths` |
| Session domain, repository contract and memory store | `aicoder_core::session` |
| Durable JSONL store | `aicoder_runtime::session::JsonlSessionRepository` |
| Completion verification contract | `aicoder_core::completion::CompletionVerifier` |
| Git workspace-change check | `aicoder_runtime::verification::WorkspaceChangeVerifier` |

## Assembly and migration

`TurnExecutor::builder` and `builder_from_shared` now start with an **empty registry**.
A custom in-memory host depends only on core and installs its own tools. To retain
coding tools in an existing embedded application, add runtime and supply the registry:

```rust,ignore
use aicoder_core::TurnExecutor;
use aicoder_runtime::{session::JsonlSessionRepository, tools::default_registry};

let executor = TurnExecutor::builder(provider)
    .workspace(workspace)
    .registry(default_registry()?)
    .approval(approval_handler)
    .build()?;
let sessions = JsonlSessionRepository::new(session_directory)?;
```

Omitting approval still denies mutating/command tools. Use a host-selected policy;
`AllowAllApproval` is available only for explicitly trusted workflows. The low-level
registry does not load project resources or remove credential environment variables.
Application hosts should normally use `Runtime::executor_builder` or
`executor_builder_for`: they assemble resource tools, context and the credential-aware
Bash policy. CLI, TUI and SWE-bench already use that path.

`ToolSuccess::new`, `ToolFailure::new` and `with_details` are public for external tool
implementations. Limits and the canonical workspace remain in `ToolContext`; path
resolution helpers moved to `WorkspacePaths::from_context`. Core still validates
workspace directory identity; it does not claim to be independent of all filesystem
operations. Bash is not a sandbox.

## External session implementations

JSONL uses only public core APIs: `Session::from_entries`, `validate_transaction`,
`apply_transaction`, metadata/entry accessors and ephemeral ownership capabilities.
Backend-specific offsets, file fingerprints and separator state stay in runtime.
`attach_backend_state(checkpoint, optional_path)` attaches a trusted backend's opaque
checkpoint; `backend_state::<T>()` inspects its concrete type. Treat it as immutable.
Cloned session views retain the old attachment when another view replaces its own,
so stale-view conflicts are preserved. `clear_backend_state` removes the attachment
and path. Reconstructing from domain entries restores neither checkpoints nor run tokens.
Checkpoint values are not rendered by session Debug; domain history never serializes them.

This changes Rust import locations and builder defaults, **not the JSONL format**.
`SESSION_VERSION` remains 3. Revisions, checksums, atomic transaction boundaries,
run leases, stale-view detection, recoverable torn tails and exact v1/v2 migration
backups retain the [existing session contract](sessions.md). Session files need no
migration solely for this crate move.

## Independent verification

Run `cargo test --locked -p aicoder-core --all-targets` separately from workspace
commands so Tokio feature unification cannot hide an accidental IO/process dependency.
Core enables Tokio macros/runtime/sync/time, without full/fs/process/network features
or a multithreaded scheduler requirement. Concrete tool, JSONL, Git and mixed-backend
contract tests live in runtime; pure execution/control/memory tests remain in core.

The coding smoke now runs as:

```sh
cargo run --locked -p aicoder-runtime --example offline_smoke
```

Core's `model_provider`, `embedded_agent`, `controlled_agent` and `event_stream`
examples remain runnable without runtime. This completes the S3 extraction item;
[Directory/glob and bounded read/search](file-tools.md) are now available.
[Prepared editing and diff review](file-mutations.md) are also available.
[Bounded shell capture and cleanup](shell-tools.md) are available.
Session approval policies remain on the [roadmap](roadmap.md).
