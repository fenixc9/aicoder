# Tool image projection

Tools return image artifacts through the existing `ToolSuccess.artifacts` or
`ToolFailure.artifacts` fields. Dispatcher-owned output envelopes promote valid top-level
`image/*` references to `ContentBlock::Image { source: ImageSource::Artifact { reference } }`
beside the anchoring tool result. JSON inside ordinary tool output cannot mint these blocks.
MCP image results use this same path; private MCP metadata remains excluded. This delivery
layer does not change the stock UTF-8 read_file tool into an image acquisition tool; that
remaining Pi difference is tracked in the [audit](pi-parity-audit.md).

Canonical history stores the original tool result and immutable reference, never decoded
preview bytes or a synthetic user turn. Artifact capture completeness describes raw storage,
not whether a model saw an image. A failed preview cannot repeat a completed tool operation.
Old records with JSON references alone remain unchanged and reference-only.

## Host assembly

Core adds no codecs, HTTP, implicit tools or storage. A host explicitly supplies
`ToolImageResolver` on `TurnExecutorBuilder::tool_image_resolver`. Its read-only result is a
`ResolvedToolImage`, which bounds and validates the inline envelope; the resolver must
validate the actual codec, dimensions and immutable source. References confer no storage or
filesystem authority. User `AgentInput` cannot contain artifact image sources.

With runtime's optional `images` feature, executor factories install
`images::ArtifactImageResolver` against the same explicitly configured workspace artifact
store used by tools. Disabling session/artifact storage leaves no implicit image store.
CLI/TUI already enable the codecs. Standalone Web hosts opt in with
`cargo build --locked -p aicoder-web --features server,host-extensions,images`; enabling
`server` alone retains its previous dependency surface. This does not add Web user-image
upload fields. Custom SDK hosts can assemble the same parts:

```rust,ignore
let executor = TurnExecutor::builder_from_shared(provider)
    .workspace(workspace)
    .registry(tools)
    .artifact_store(store.clone())
    .tool_image_resolver(Arc::new(ArtifactImageResolver::new(store)))
    .build()?;
```

The runtime resolver reads at most 8 MiB per source in at most 64 KiB pages. Every page must
match the complete reference, expected offset and continuation. It never loads a path or URL
from a result. Actual PNG/JPEG/static GIF/static WebP signatures must match the declared MIME.
The existing optional bounded decoder checks dimensions/pixels, orientation and animation,
then produces a JPEG preview with at most 1600 pixels per side and 256 KiB encoded bytes.
Preview normalization can reduce quality/resolution. Raw captured bytes stay unchanged.
Decoder allocation limits retain the [local image contract](image-input.md)'s documented
limitations; this is not an OS memory sandbox.

## Provider views and budgets

The shared pure projection used by model selection, preflight, context policies and pruning
keeps at most the newest four tool images per request. Four normalized previews admit at most
1 MiB of decoded JPEG bytes, plus base64/framing. Older images remain in canonical history.
User-selected images retain their separate existing limits and strict capability validation.

The model-view tool result's reserved `_tool_image_projection` field describes each preview
or omission using bounded status data: `normalized_preview`, `model_without_vision`,
`incomplete_source`, `image_limit`, `unavailable`, `invalid_image` or `image_timeout`. It is not
canonical history or authorization. No visual support is inferred from a model name. Models
without declared image support receive references plus the omission status; incompatible
user image input still rejects. Missing stores/artifacts and invalid codecs omit only the
preview. Resolvers have a two-second per-image deadline; turn cancellation, run duration and
dropped futures cancel their owned child contexts before another model request can start.
Bounded blocking codec work can finish before it notices cancellation but cannot publish late.

The compatible adapter sends all matching tool results before a provider-only user message
with the corresponding inline image parts. It flushes this view before the next ordinary
message or at request end. Chat Completions tool-role content accepts text, while user
content accepts images; see the [official request schema](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create).
Unresolved artifact sources fail before HTTP encoding; direct adapter users must supply
resolved bytes themselves. Provider validation accepts descriptors for read-only selection
checks and does not promise the stored image is available until execution resolves it.

Image bytes/base64 are excluded from text tokenization. The current 8192-token per-image
heuristic is an explicit estimate with unsupported-content fallback, not exact provider
framing, vision tokens or billed usage. Final materialized counts, including delivery
observations, are checked against known model windows and run token limits again. A custom
resolver/counter cannot turn declared metadata into exact service usage. Semantic summaries
omit pixels and preserve raw history under the existing summary contract.

## Offline verification

```sh
cargo test --locked -p aicoder-core tool_images --lib
cargo test --locked -p aicoder-runtime --all-features --test images --test mcp
cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web -p aicoder-runtime --features server,host-extensions,mcp-fixtures,images --bins
python3 scripts/check_host_extensions.py --images
```

The MCP integration uses an actual local stdio peer, two parallel image-returning calls, a
loopback HTTP adapter, runtime-scoped disk artifacts and JSONL session replay. It decodes the
wire JPEG parts and checks tool-result ordering, original references, absence of synthetic
user history/private metadata and no repeated calls on replay. Separate execution cases
cover a text-only model and corrupt image bytes without changing successful raw capture.
These fixtures prove delivery contracts, not a real model's visual reasoning quality.
