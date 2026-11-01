# Shared runtime configuration (S3 increment)

`aicoder-runtime` now owns application configuration, credential references, default
agent/tool/session assembly and context-pruning setup. CLI, TUI and the SWE-bench
entry point use it. Core keeps the single execution loop and does not gain a runtime,
HTTP or frontend dependency. [Builtin tools, JSONL storage and Git verification](runtime-backends.md)
now live in runtime; core builders use an empty tool registry unless one is supplied.
Remaining tool improvements are open S3 items.
[Project instructions, skills and templates](project-resources.md) are now available.

## Files and precedence

Settings resolve **per field** in this order:

1. Explicit SDK/command-line settings.
2. Process environment, then an explicitly selected dotenv fallback.
3. Workspace `.aicoder/settings.json`.
4. `$AICODER_HOME/settings.json`, or `$HOME/.aicoder/settings.json`.
5. Shared defaults, some derived from the selected service or output capacity.

Missing fields inherit. JSON null explicitly clears nullable fields, such as task
budgets, context-window enablement, model options and the shared system prompt.
Required fields like max_rounds/stream cannot be null. Structured response-format
and event-delivery values replace their whole field rather than merging subproperties. Nullable environment settings
accept the literal `null`. Process values win dotenv fallbacks, including supported
model/API-base aliases; an empty process value suppresses that dotenv variable and is
otherwise treated as unset. Within one source, AICODER names precede legacy aliases.
Higher explicit values avoid parsing overridden environment values.

Limits in user/project files are configurable defaults, not immutable policy ceilings.
Use explicit or environment overrides for a host's final execution limits. Approval
policy is injected by the host; settings files cannot enable automatic approvals.

A user file can define trusted service profiles and storage:

```json
{
  "version": 1,
  "profiles": {
    "work": {
      "provider_id": "work",
      "base_url": "https://gateway.example/v1",
      "api_key_env": "WORK_API_KEY",
      "model": "coding-model",
      "proxy": "disabled",
      "timeout": 120
    }
  },
  "settings": {
    "provider_profile": "work",
    "options": {"max_output_tokens": 4096},
    "execution": {"max_rounds": 8, "max_tokens": 12000}
  },
  "session_dir": "sessions"
}
```

A workspace file selects that profile and overrides task settings:

```json
{
  "version": 1,
  "settings": {
    "provider_profile": "work",
    "execution": {"max_rounds": 12, "max_tokens": null},
    "context": {"window_tokens": 65536}
  }
}
```

Project files cannot define profiles or choose/disable session storage. This keeps
endpoint/credential routing and private transcript paths in trusted user/host settings.
A project may select a profile already defined by the user. Profile definitions require
an endpoint and valid environment reference; an empty reference cannot fall back to an
unrelated native key. API keys are not accepted as profile/settings fields. Selected
file profiles also reject proxy URL credentials; supply those through environment or
an explicit host descriptor instead. Routing environment overrides still precede file
profile defaults, even when the profile name was explicitly selected.

`version: 1` is required. Unknown fields, unsupported versions, invalid effective
values and files over 256 KiB fail. Errors identify the path/line/column without echoing
file content or parsed credential values. Only regular files are read; symlinks are
resolved and duplicate canonical user/project paths are loaded once as the user file.
JSON configuration is workspace-only; [resource discovery](project-resources.md) has
separate parent/nested rules.

`--config PATH` and `--user-config PATH` choose alternative files. Runtime discovery
paths and explicit/env storage paths are relative to the resolved workspace; session
paths in a user JSON file are relative to that file. Existing-file provenance uses
canonical paths. Defaults do not create directories while inspecting settings.
`--no-config` disables JSON file discovery, not process variables or an explicit dotenv
input. A null user session_dir disables the default store; stateless CLI/SDK execution
can still use no store or an injected one. TUI currently requires session storage.

## Credentials, environment and introspection

```sh
aicoder config
aicoder --config ./settings.json --max-rounds 10 config
aicoder --env-file /path/to/trusted.env --prompt 'inspect this project'
aicoder-tui --show-config
aicoder-tui --profile work
```

Automatic `.env` searches and compile-time crate-directory fallbacks have been removed.
All three entry points now use explicit `--env-file PATH` if dotenv input is wanted.
That path is a normal CLI file argument relative to the invocation directory. Its
contents are a private fallback environment and never modify process variables or get
exported to Bash. Duplicate variables or malformed/oversized/non-regular dotenv input
fail without echoing values. Interpolation follows dotenvy's existing parser behavior.
Explicit dotenv values are snapshotted for this runtime; process credential references
are looked up again per HTTP attempt. Reloading JSON settings does not reread the
already constructed dotenv source; construct a new source to reload it.

Resolved configuration contains a credential variable name, never a resolved bearer
value. Native automatic service selection uses only presence/emptiness for routing; it
does not construct or retain a bearer token. Explicit references are not resolved
until credential use. Missing/invalid
credentials therefore need not prevent configuration/session inspection; invalid
routing/settings still fail validation. Executing a
request still validates them. `credential_provider()` supplies that separate source,
`client_with_credentials` accepts a host's async vault/refresh implementation, and
`executor_builder` accepts any core ModelProvider. Native OpenAI/DeepSeek selection and
transport validation reuse the provider resolver. Available unrelated native keys do
not replace the credential reference of a selected profile.

Provider transport variables retain [their S1 meanings](provider-configuration.md).
Standard HTTP(S)/ALL_PROXY/NO_PROXY in auto mode still come from Reqwest's **process**
environment. A host snapshot/dotenv source does not emulate global Reqwest proxy
variables; use AICODER_PROXY for an explicit mode/URL when needed.

New runtime variables are:

| Variable | Effective field |
| --- | --- |
| AICODER_PROFILE / AICODER_MODEL | Profile selector / model (legacy model aliases remain) |
| AICODER_MAX_OUTPUT_TOKENS | options.max_output_tokens |
| AICODER_TEMPERATURE / AICODER_TOP_P / AICODER_SEED | Optional sampling parameters |
| AICODER_MAX_ROUNDS / AICODER_STREAM | Execution rounds / streaming |
| AICODER_MAX_TOOL_CALLS / AICODER_TOKEN_BUDGET | Run quotas |
| AICODER_TIME_BUDGET_MS / AICODER_OUTPUT_BUDGET_BYTES | Duration in ms / generated bytes |
| AICODER_CONTEXT_WINDOW | Optional total context capacity |
| AICODER_CONTEXT_STRATEGY | `prune` (default) or `semantic`; semantic requires a window |
| AICODER_SUMMARY_OUTPUT_TOKENS | Summary output capacity, 1–8192; default 1024 |
| AICODER_RESERVED_OUTPUT_TOKENS / AICODER_PRESERVE_RECENT_TOKENS | Pruning capacities |
| AICODER_SYSTEM_PROMPT | Shared base system prompt; null clears it |
| AICODER_SESSION_DIR | Explicit transcript directory |

Common defaults are 8 rounds, streaming enabled, no task quotas, 4096 output tokens,
no context pruning until a window is configured, and a concise coding prompt in the
user's language. Reserved output capacity derives from max_output_tokens (4096 when
unset); recent preservation defaults to a fitting value up to 8192. Invalid windows or
output reservations fail. Provider-specific option capability checks still happen in
core/the adapter. Stop/response-format and event-delivery settings are available through
JSON/SDK, not new environment encodings.

`RuntimeConfig.sources` identifies each layered setting's explicit/env/env-file/user/
project/default origin without including secret values. Workspace/schema and the union
of known credential reference names are derived metadata. Inspect output is an effective
snapshot, not a reloadable settings-file backup. Debug/serialization redact API/proxy
credentials. Normal operation does not print the snapshot automatically.

Runtime default Bash assembly removes the selected and all configured user-profile key
reference names in addition to the existing managed exclusions. Dotenv-only secrets
are never process-exported. This is an inherited-environment policy, not isolation from
files, other unconfigured secret aliases or the user's permissions; Bash remains outside
a sandbox. Hosts replacing the tool registry own their environment policy.

## Host and benchmark assembly

```rust,ignore
let runtime = Runtime::load(workspace, RuntimeOverrides::default())?;
let provider = Arc::new(runtime.client()?);
let executor = runtime.executor_builder(provider)?.approval(my_policy).build()?;
let agent = Agent::new(executor, runtime.agent_config());
let sessions = runtime.sessions()?;
```

`load_with_environment` injects a snapshot/refreshed lookup and discovery policy without
global environment mutation. Factories can be overridden with custom tools, approval,
compaction/verifiers, model providers and memory/custom stores. The HTTP descriptor is
used by `client`; an injected non-HTTP ModelProvider owns its own service/credential
semantics and must support the configured/requested model and options.

`reload()` produces a new immutable effective snapshot. Existing agents retain their
old model/options/budgets; chosen process credential values may rotate independently
through the same variable reference. No active run is reconfigured automatically.
[Resource snapshots](project-resources.md) provide separate resource-only reload and
TUI source/reload commands.

SWE-bench freezes the invocation workspace's effective settings, records their non-secret
snapshot in parameters, and applies them to every case executor/request. Case tool
workspaces remain isolated; task repository settings do not silently change the frozen
model midway through a batch. Benchmark-specific engineering instructions are appended
to the shared base prompt; no grading patch/answers are added. Different effective
parameters invalidate checkpoint matching as before. S3's later resource integration
must preserve explicit provenance and grading-data separation.

```sh
cargo run --locked -p aicoder-runtime --example offline_runtime
```

This uses an injected scripted model and memory store with no credentials/network.
The contract tests cover precedence/aliases, null vs missing, source tracking, redaction,
refreshable credentials, regular-file/size guards, reload snapshots, explicit dotenv,
managed shell exclusions and CLI/TUI inspection without a terminal or session writes.


The shared per-tool deadline is `settings.execution.tool_timeout_ms`, or
`AICODER_TOOL_TIMEOUT_MS` (default 30000; positive, at most one day). It applies to normal
and isolated-case factories, is visible in `sources`, and is separate from the enclosing
run-duration budget. Core phases and shell calls still obey the shorter applicable
limit. See [the shell contract](shell-tools.md) for command/output limits and cleanup.

Semantic compaction and the CLI/TUI manual commands share the
[summary contract](semantic-summary.md). Automatic pruning remains the default.

Output archives use the trusted session root with workspace scoping and lazy creation.
Clearing session storage or CLI `--no-session` disables persistent archives. The
[artifact contract](output-artifacts.md) covers SDK policy limits, retention and the
credential-free `read-artifact` command.

Session continuation can follow the selected branch model through `Runtime::session_client`,
using the fixed trusted service profile and shared HTTP pool. Explicit CLI model/profile
selection overrides continuation; history never chooses credentials or endpoints. Local
model compatibility checks and the new metadata commands are described in
[session navigation](session-navigation.md).

`context.tokenizer` / `AICODER_TOKENIZER` selects `heuristic` (default), `auto`,
`cl100k_base` or `o200k_base`. Explicit BPE requires the optional `tokenizer-tiktoken`
Cargo feature; auto reports estimated fallback when a hint/encoder is missing.
[Token counting and ContextPolicy](context-policy.md) separates text precision, request
framing estimates and provider-reported usage.


## Optional Code Mode

The runtime code-mode build feature adds a bounded QuickJS tool, disabled until code_mode.enabled
is true. CLI/TUI/Web --code-mode opt in through that same setting. CodeModeSettings.tools
selects exact frozen registry names; null/inherit offers all non-composite tools, while []
offers none. Environment equivalents are AICODER_CODE_MODE and AICODER_CODE_MODE_TOOLS (a JSON
array or null). Ordinary per-field precedence applies. Builds without the feature reject an
enabled setting before provider IO. No JS program or public branch store can install tools,
route credentials, change those limits or supply authorization rules. See [Code Mode](code-mode.md)
for SDK registration, bounds, output, leaf approvals and canonical record semantics.
