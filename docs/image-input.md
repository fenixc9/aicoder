# Bounded image input (S5)

Images enter the same core user-message path as text. AgentInput accepts strings for
compatibility and validates explicit Message inputs as user text/image blocks only. It
does not convert images to system instructions, tool results, approval choices or paths for
later implicit reads. All stateless/session/controlled Agent entry points use that input;
RunHandle already accepts the same user Message type for steer/follow-up.

## Local capture and budgets

The optional runtime images feature owns concrete image IO/normalization; CLI/TUI enable
it, while default core/runtime builds do not require a codec. Local files must resolve
inside the host workspace, be regular files, and pass identity/size/time checks across the
source read. Reads,
source bytes, dimensions/pixels, decoded bytes, encoded output, image count and serialized
user messages are bounded. Decoders receive a 64 MiB allocation request limit; some codec
internal allocations do not enforce it strictly, so this is not an absolute process RSS cap
(see [image::Limits](https://docs.rs/image/0.25.10/image/struct.Limits.html)).
Captured normalized pixels/bytes are immutable. Submission and
historical replay never re-open the file. Codec work is host IO, not agent execution.

PNG, JPEG, GIF, WebP and BMP inputs are normalized to static JPEG, including declared orientation,
white-background RGB conversion, aspect-preserving resize and at most five quality/size
attempts (q80/60/40 at max side, then q60 at half/quarter side). The original file
is not modified; metadata reports its source size/fingerprint, original/output dimensions,
normalization and selected quality. Static input is the supported modality; animated sources
are rejected when identified. No raw original or private EXIF metadata is implicitly archived.

Default capture admits at most 8 MiB source, 16 megapixels/8192 per dimension, a 1600-pixel
output side and 512 KiB encoded JPEG. CLI can share a 1 MiB normalized budget among up to four
images. TUI stages up to four / 1 MiB normalized images. Image-bearing user messages are
limited to 2 MiB minus 16 KiB serialized data, reserving metadata headroom for canonical
window/record budgets. TUI explicitly admits up to 2 MiB per queued input / 4 MiB pending;
core RunControl defaults remain 64 KiB / 256 KiB. Other hosts choose their own queue limits.
Text-only legacy input is not silently subjected to new image limits. Oversized/invalid input
fails explicitly; normalization may reduce resolution/quality and is reported to the user.

## Frontends

```sh
aicoder --image screenshot.png --print 'explain the failure'
aicoder --image left.png --image right.png --json 'compare these'
aicoder --image screenshot.png --print
```

CLI --image is a repeatable local file argument. Without text, it submits an image-only
user message. In REPL it belongs only to the initial turn; later input does not silently
resend pending files. It cannot be combined with RPC/metadata commands. Logs, file notices
and normalization reports stay on stderr; print/JSON stdout retains its declared format.

TUI /image PATH captures asynchronously, including while a model is running. Pending image
labels show dimensions/normalization; /unimage [INDEX|all] removes them. Enter sends an
image-only prompt if no text is present. Images are included in an accepted turn/steer/
follow-up, cleared only after admission, and retained on rejection. Navigation/cancellation
reject late captures. Dropping capture cancels its child context; bounded CPU decoding can
finish before noticing cancellation, but cannot attach a late result. Paste cannot authorize
or automatically submit an image.

## Capabilities and counting

ModelCapabilities.images remains explicit host metadata (for example a trusted profile's
models entry). Unknown/default generic models do not acquire image support from their
name. Core/adapter capability validation precedes user/turn journal writes or service calls;
resuming an image branch with an incompatible model fails rather than dropping images.
The default allowance is 8192 estimated tokens per image, independent of base64 length.
The existing compatible adapter maps user Image blocks to image_url parts; data stays in
canonical history and provider projections preserve it. URL input remains an SDK-owned
source; local capture never fetches remote images. URL bytes are not immutable pixel snapshots
and may change remotely; that source remains the caller's responsibility.

Image transport bytes/base64 are not text-token or billed vision usage. The default image
heuristic charges an explicit estimated per-image allowance and marks unsupported-content
fallback; provider-specific counters may replace it. It is not an exact visual token meter
or hard billed-usage guarantee. Text encoders fall back explicitly for image content. The
same count is used for preflight, pruning and budgets; known windows/protected units remain
enforced. Normalization/counting does not increase authorization or configured run budgets.

[Acceptance](s5-images-acceptance.md) records codec, SDK, canonical replay, CLI/PTY and
footprint evidence. Image upload/remote URL input in the minimal Web/RPC interfaces is not
introduced by these local CLI/TUI commands; other hosts use AgentInput/Message directly.

## SDK and offline example

Strings continue to work. For multimodal turns, construct a user Message and validate it
with AgentInput::try_from(message), or call runtime images::input(text, &attachments).
All Agent run methods accept that owned input; no model request or file load is hidden in
its conversion. The lower-level ModelRequest remains available to custom hosts.

```sh
cargo run --locked -p aicoder-runtime --features images --example offline_images
```

The example captures a PNG, modifies the original file and verifies exact captured input
in both the provider request and JSONL session, using a scripted provider without credentials.
Runtime images is optional; CLI/TUI enable only PNG/JPEG/GIF/WebP/BMP codecs, without other image
formats, Rayon or tokenizer vocabularies. Source reads and codec failures never become tools.

## Tool output images

Tool and MCP images use scoped artifacts rather than user input. Runtime can validate and
normalize those artifacts into bounded model-only previews; canonical history keeps the
original references. Non-vision models receive explicit preview omissions. See the separate
[tool image contract](tool-images.md) for limits, ordering, storage and migration.

Native agent-selected image files now use [read_file](file-tools.md#native-image-files),
including BMP, through the same scoped artifact/preview path. Those raw snapshots include
all original bytes; the user-selected local capture path above retains normalized pixels.
