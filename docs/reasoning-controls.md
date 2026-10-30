# Provider-neutral reasoning controls (S5)

ReasoningEffort is a typed model option: none, minimal, low, medium, high, xhigh or max.
An omitted option selects the service default; explicit none asks to disable reasoning
and requires that exact declared capability. ModelCapabilities.reasoning_efforts is an
explicit list. Empty means no declared control support, not unlimited support. Unsupported
levels fail before a turn/selection is committed or a service request is sent. Reasoning
history remains a separate capability: visible prior analysis does not authorize an option.

The OpenAI-compatible adapter maps the option to reasoning_effort, and generic requests
for models declaring reasoning controls use max_completion_tokens for the logical output cap. The mapping
follows [official OpenAI documentation](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create).
This does not infer availability from model names or claim all compatible servers implement
all values. Trusted provider profiles can register per-model metadata. DeepSeek compatibility
profiles preserve their existing behavior: thinking declares high, chat declares none; other
levels are rejected rather than silently coerced.

## Host configuration

Trusted user profiles may include a bounded models array of provider-neutral ModelSpec
objects. Provider identity must match the host-selected route. Project settings may select
a profile and model but cannot install routing descriptors or register trusted models.
No credential value is stored in metadata/config snapshots. SDK callers may still inject
ModelProvider or register metadata on ChatClient directly.

options.reasoning_effort in layered settings and AICODER_REASONING_EFFORT choose the host
option; null/--reasoning-effort default clears it. CLI/TUI expose --reasoning-effort and TUI
/thinking [LEVEL|default]. Default clears an explicit effort; none is an explicit level.
Configuration provenance distinguishes an omitted default from an explicit clear. Host
configuration overrides recorded choices in CLI/RPC/Web; otherwise those frontends opt into
following the selected branch. TUI uses configured values as initial fallbacks and follows
saved/interactive choices; explicit --session plus --reasoning-effort validates and records
the startup choice once. Before a first TUI turn, /thinking stages a validated choice in
the composer; it is bound to that new conversation and cleared on session navigation.

## Sessions and compatibility

A small aicoder.reasoning.v1 extension records provider/model identity plus effort (nullable
for service default). It is metadata, never a model instruction, credential route, restored
permission, or copied assistant reasoning. Selection validates the projected context and
adapter capability under the session run lease before appending. Turn preparation also
validates before writing its user/turn entries. Canonical history remains unchanged by
projection repairs and summaries.

Choices are branch-local and bounded by a model-selection epoch. Selecting a different
model does not silently reuse an older model's effort; returning to a model starts from
host defaults unless a new validated choice is made. Switching a model and its effective
reasoning choice is one atomic transaction. Window/archive views retain the small metadata
without materializing old payloads. Recorded choices affect execution only through an
explicit host opt-in and its fixed provider route.

Absent options preserve prior wire defaults and JSON representations. Session JSONL and
event versions stay unchanged because the record uses the existing extension mechanism.
New Rust fields require explicit struct-literal callers to add ..Default::default() or the
new fields; intentional migration details and verification accompany the implementation.
This implements the reasoning-control portion of S5; the richer TUI item remains open.

Automatic/manual semantic summaries retain their separate bounded, tool-free generator policy
and service defaults; a coding-turn effort is not silently copied to a different summary model.

Example trusted user profile (no credential value in this file):

```json
{
  "version": 1,
  "profiles": {
    "reasoning": {
      "base_url": "https://gateway.example/v1",
      "api_key_env": "GATEWAY_KEY",
      "provider_id": "gateway",
      "model": "coding-model",
      "models": [{
        "provider": "gateway", "id": "coding-model",
        "capabilities": {"tools": true, "reasoning_efforts": ["low", "high"]}
      }]
    }
  },
  "settings": {"provider_profile": "reasoning"}
}
```

```sh
aicoder --reasoning-effort high --print 'inspect the project'
aicoder set-session-reasoning SESSION_ID low
aicoder set-session-reasoning SESSION_ID default
aicoder-tui --reasoning-effort low
cargo run --locked -p aicoder-runtime --example offline_reasoning
```

The profile is a host declaration, not a statement about that illustrative endpoint.
Models used with default coding tools must declare tools support too. Output/context
reservations and token-count accuracy/fallback are unchanged; higher effort does not grant
more tools or silently raise run budgets. ModelContextInfo.reasoning_effort records the
requested coding option, not actual reasoning work or billed cost. Old JSON without the
optional field remains valid; strict external event-schema consumers should update the
published v1 schema for this additive metadata.

SDK callers can use Agent::with_reasoning_effort to vary request preferences over the same
live executor/tool registry. The fork overrides saved reasoning, persists an explicit default,
and preserves live registry identity instead of rebuilding it. Saved reasoning is opt-in via
following_session_reasoning; default Agent construction retains host-configured behavior.
Explicit metadata changes invalidate TUI's ephemeral session grants, while ordinary request
preference forks do not reconstruct or restore authority. The TUI awaits operation owners
on terminal exit/errors before its runtime is destroyed.

See [acceptance](s5-reasoning-acceptance.md) for typed API, journal, codec and actual PTY tests.
