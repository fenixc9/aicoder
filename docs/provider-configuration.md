# Provider profiles and HTTP configuration

`aicoder-providers` supplies an OpenAI-compatible HTTP adapter; core still
knows only `ModelProvider`. CLI, TUI and SWE-bench share the same environment
resolver and do not guess a provider from a model-name prefix.

## Environment selection

Credentials are selected in this order:

1. `AICODER_API_KEY_ENV` names another environment variable containing the key.
2. `AICODER_API_KEY` supplies a generic compatible-service key.
3. `OPENAI_API_KEY` supplies an OpenAI key.
4. `DEEPSEEK_API_KEY` supplies a DeepSeek key.

Blank optional values are treated as unset. An explicit reference to a missing
key fails. Anthropic/DashScope/Zhipu variables are no longer automatically sent
to the OpenAI endpoint. To use a compatible gateway with a differently named
secret, explicitly configure its reference and endpoint.

`AICODER_API_BASE` overrides the legacy generic alias `OPENAI_API_BASE`.
Without a generic endpoint override, the OpenAI key uses
`https://api.openai.com/v1`; the DeepSeek key uses `DEEPSEEK_API_BASE` or
`https://api.deepseek.com`. The DeepSeek endpoint follows the
[official OpenAI-compatible quickstart](https://api-docs.deepseek.com/).
Generic credentials require an explicit base URL.

An explicit SDK/TUI/bench model argument wins, then `AICODER_MODEL`, then
`OPENAI_MODEL`, and then `DEEPSEEK_MODEL` for DeepSeek profiles. The built-in
OpenAI endpoint keeps the existing `gpt-4o` default; DeepSeek profiles use
`deepseek-flash`. Custom gateways require a model instead of inheriting an
unrelated service's model name. These are client defaults, not a live model
availability catalog.

`AICODER_PROVIDER_ID` optionally names a logical profile. Environment-created
OpenAI/DeepSeek profiles default to `openai`/`deepseek`; a direct SDK config
keeps `openai-compatible` by default. Registered model metadata must use the
client's profile ID. This lets hosts keep separate gateways/accounts in one
model registry.

| Variable | Default | Scope |
| --- | --- | --- |
| `AICODER_PROXY` | `auto` | `auto`, `disabled`/`no-proxy`/`none`, or explicit HTTP(S) proxy URL |
| `AICODER_HTTP_TIMEOUT` | 120 | Complete HTTP response timeout, seconds |
| `AICODER_CONNECT_TIMEOUT` | 10 | Connection timeout, seconds |
| `AICODER_MAX_RETRIES` | 3 | Additional HTTP attempts; zero disables retries |
| `AICODER_STREAM_USAGE` | `true` | Set `false` for endpoints rejecting `stream_options` |
| `AICODER_MAX_RESPONSE_BYTES` | 33554432 | Successful body ceiling; includes SSE framing |
| `AICODER_COMPATIBILITY` | Selected from service/profile | `generic`, `deepseek-thinking`, or `deepseek-chat` |

HTTP timeouts and body ceilings must be positive. Invalid numeric/boolean
settings fail before requests. Configuration no longer hard-codes proxy
bypass: use `AICODER_PROXY=disabled` to retain the old direct-connection behavior.
Auto delegates standard HTTP(S)/ALL_PROXY/NO_PROXY handling to Reqwest; explicit
mode overrides system proxy selection and bypass rules. Explicit proxies
support HTTP(S), with optional Basic authentication; SOCKS is not enabled.

Base URLs are path prefixes, not full `/chat/completions` request URLs. Trailing
slashes are normalized. Userinfo, query parameters and fragments are rejected
in API base URLs. Redirects are not followed. Native Azure/Anthropic protocols
need their own adapters; their authentication is not inferred from key names.

## SDK and refreshable credentials

```rust,ignore
let config = ClientConfig {
    provider_id: "work-gateway".into(),
    base_url: "https://gateway.example/v1".into(),
    model: "coding-model".into(),
    api_key_env: Some("WORK_MODEL_TOKEN".into()),
    proxy: ProxyMode::Disabled,
    ..Default::default()
};
let client = ChatClient::new(config)?;
```

A nonempty `api_key` takes precedence over `api_key_env`. Inline keys become a
private `StaticCredential`; an env-only descriptor reads its variable per
attempt. `ChatClient::with_credentials(config, Arc<dyn CredentialProvider>)`
injects an asynchronous token source, suitable for host-owned key stores or
refreshable bearer credentials. It is called for each attempt, so retries can
use a refreshed key. Simple synchronous closures also implement this trait.

`BearerToken` validates headers, marks authorization values sensitive and
redacts Debug output. Explicit `expose_secret()` access is available to
custom authentication adapters. The model cancellation context covers an
awaited credential callback; HTTP timeout settings start with the HTTP request,
not arbitrary work inside that callback.

`ClientConfig::from_environment` accepts a host lookup function. It resolves
and validates a snapshot without modifying process environment, and ignores
lower-priority overridden values. `ChatClient::from_env_model(None)` resolves
a default profile/model; `from_env(model)` retains the explicit-model shortcut.

Config Debug and JSON snapshots omit inline/proxy credentials. Snapshots retain
environment reference names and public URLs; they are not secret backups. A
snapshot without a reference needs a key supplied again. Proxy URL userinfo
is decoded into private auth fields and omitted from snapshots.

## Compatibility profiles

`generic` declares a common chat-completions profile. It does not promise every
option is implemented by every model; `register_model` can supply precise
capabilities/limits. Images and reasoning history remain opt-in in this profile.

Known DeepSeek endpoints or DeepSeek credentials select `deepseek-thinking`.
It explicitly enables thinking at high effort and preserves assistant reasoning
in subsequent tool turns. It also sends empty reasoning fields for assistant
history that had no reasoning text. This follows the
[thinking-mode tool-history requirement](https://api-docs.deepseek.com/guides/thinking_mode/).

The thinking profile rejects temperature, top-p, seed, named/required tool
selection and JSON Schema output instead of silently changing/ignoring them.
JSON-object output remains available. `deepseek-chat` explicitly disables
thinking; temperature and named/required tool selection are available, while
top-p/seed remain conservatively disabled. Model-specific overrides can declare
more precise sampling capabilities. The response-format/tool-choice scope is
based on the [chat-completions reference](https://api-docs.deepseek.com/api/create-chat-completion/).

All frontends now leave sampling parameters unset by default. SWE-bench's
`--temperature` is optional and recorded as null when omitted. Use an explicit
supported temperature when a benchmark requires it; do not label an ignored
thinking-mode setting as a controlled experiment. Effort-level UI controls
remain later frontend work, not an additional provider catalog in S1.

## Responses and diagnostics

Streaming and non-streaming calls share HTTP status classification. Error
parsing accepts nested OpenAI/DeepSeek objects, flat compatible objects,
string errors and plain text. `ProviderError` retains HTTP status plus optional
string/numeric vendor `code` and `parameter`. Request timeouts have code
`timeout`; malformed successful payloads/body-limit violations are invalid
responses. Transient errors still use Retry-After and bounded exponential
backoff; stream data is not replayed after output has begun.

Error bodies are read only up to 8 KiB; rendered messages are bounded and retain
a truncation marker. Successful responses obey `max_response_bytes`; the SSE
parser preserves complete prefix events before rejecting an oversized stream.
The per-event SSE limit also remains in effect. This is separate from the
executor's logical output budget.

Exact active bearer tokens, proxy credentials and common credential patterns
are scrubbed from diagnostics, including parser errors and backend metadata.
Configured/system proxy credentials are not included in public snapshots.
Default DNS uses the system resolver; unused Hickory/Moka dependencies have
been removed. Bench workers reuse an immutable provider/HTTP pool rather than
recreating transport settings for every case.

Shell commands now capture stdout/stderr into tool results, use closed stdin
and run non-login Bash with inherited PATH. Known provider key variables,
`AICODER_API_KEY_ENV`'s referenced key, app proxy credentials and `BASH_ENV` are
removed from the child environment. This does not create a shell sandbox or
replace the broader environment policy planned in S3.
