# Native auxiliary classifiers

Auxiliary classifiers answer typed bool/choice/score questions about bounded JSON state.
They use their native System One endpoint, not an arbitrary chat request or a second agent
loop. Core owns typed contracts, token admission, cancellation and usage records. Providers
own HTTP/credentials; runtime owns trusted model routes and immutable prepared tools.
[Image generation](image-generation.md) uses the same meter through a separate typed
request/provider/delivery contract.

## Host configuration

Standalone SDK/runtime users enable `auxiliary-models`. The optional `code-mode` runtime
feature includes it; default core/runtime builds add no service or JS client. Stock CLI/TUI/
Web hosts use their existing code-mode build feature and opt-in settings. Explicit trusted
user profiles declare classifiers; project files may select those profile names but cannot
define routes, credentials or storage. Nothing is loaded from session state as authority.

```json
{
  "version": 1,
  "profiles": {
    "judge": {
      "provider_id": "typesafe",
      "base_url": "https://api.typesafe.ai/v1",
      "api_key_env": "TYPESAFE_API_KEY",
      "proxy": "disabled",
      "classifier_image_input": "assets",
      "classifiers": [{
        "provider": "typesafe", "id": "jev-latest", "name": "Jev",
        "images": true, "context_tokens": 65536, "output_reservation": 1024
      }]
    }
  },
  "settings": {
    "code_mode": {
      "enabled": true, "only": true, "model_profiles": ["judge"]
    },
    "execution": {"max_auxiliary_calls": 16}
  }
}
```

The main agent still uses its independently selected chat profile. model_profiles defaults
to empty and admits at most 16 trusted profiles / 128 native classifier/image entries. Native route settings
are private and omitted from RuntimeConfig serialization; snapshots contain only selected
profile names and normal credential reference names. Values never enter previews/errors.
AICODER_MODEL_PROFILES is a JSON list equivalent; AICODER_MAX_AUXILIARY_CALLS controls the
optional run ceiling. Model/route duplicates, wrong provider identities and unknown profiles
reject. Credential values are resolved per HTTP attempt from the selected host route.

System One posts to base_url/systemone. Bool questions map to wire noul, while choice/score
retain their native criteria. The host explicitly chooses classifier_image_input: assets
(TypeSafe native with experimental image opt-in header), state_image_urls (OpenRouter-style
state image parts), or images (data URI list). This is not inferred from names/hosts.
Only data images are sent; task input URLs are rejected. Native assets accept PNG/JPEG;
state/list forms also accept WebP. Artifact input uses the current scoped store and optional
images decoder to create an immutable bounded JPEG input before approval. Codec-free hosts
can provide bounded inline image blocks but cannot implicitly load artifact pixels.

## SDK and Code Mode

Core exposes ClassifierModel, ClassifierContext, typed questions/answers, ClassifierProvider,
AuxiliaryOperation and a request-time usage reporter. A PreparedTool can declare an immutable
auxiliary operation. The normal dispatcher retains the plan, obtains ordinary tool approval,
then performs bounded parent-run admission and invokes it once. Core builders install no
implicit tools. Direct unmetered prepared-tool execution rejects. Low-level provider clients
remain available to hosts that explicitly own their own metering.

Runtime registers model_catalog and model_classify when selected routes exist; Code Mode
also offers the read-only catalog when no classifiers are selected. Model catalog entries
contain type/provider/id/name/input metadata and configured credential availability, never
URLs or keys. Available means the configured reference is nonempty, not a live service probe.
Chat models can be listed but cannot be executed as classifiers. Image models use the separate
[image-generation contract](image-generation.md). Registry collisions fail closed.

```js
const judge = await models.getModelOfType("classifier", "typesafe", "jev-latest");
const result = await models.classify(judge, {
  state: {text: "The test failed after a timeout"},
  questions: {
    needsAttention: {type: "bool", instructions: "Does this need attention?",
      criteria: {true: "failure or incomplete work", false: "complete success"}},
    category: {type: "choice", instructions: "Choose the category",
      criteria: {failure: "failed work", success: "successful work"}},
    severity: {type: "score", instructions: "Rate severity",
      criteria: ["low", "high"]}
  }
});
if (result.stopReason === "stop") text(result.answers);
else text(result.errorMessage);
```

models.getModelsOfType(type, provider?), getAvailableOfType and getModelOfType query the
current host catalog. classify uses only provider/id from the model object. It returns a
stop/error result for ordinary provider/tool errors; parent cancellation/budget termination
still stops the owning run. Error results retain reported usage when available. Questions,
state and images are data, not routes or authorization. Explicit Code Mode leaf subsets must
allow model_catalog/model_classify. Root only mode continues to declare codemode alone.

State/question metadata is admitted at most 64 KiB; up to 32 questions, 32 choice/score
criteria, 4096-byte instructions/descriptions and 64-byte question/label IDs are accepted.
Images are at most four bounded inline/host snapshots. Choice labels/distributions must match
criteria and sum to one within 0.001; probabilities/confidence are finite in [0,1], score is
within the declared level range and answer IDs/types must match. Invalid output never becomes
a fabricated valid answer. Complete result JSON must fit the current tool budget; oversized
completed results report delivery failure rather than repeating inference.

## Shared native transport

[Runtime transport bindings](auxiliary-transport.md) explicitly select the same immutable
HTTP callback snapshot for created primary/classifier/image clients. Native input and image
protocol opt-ins stay owned by the approved plan; response observations cannot alter usage
or normalization. Bindings remain host objects, never profile/history configuration.

## Usage, limits and history

Classifier calls are conservative Command tools, independently approved even under Code Mode.
Host-declared independent operations may run in parallel without changing their approval
requirements; existing mutation/command tools retain serial defaults. There are four task
slots per run and a hard ceiling of 128 task admissions, further bounded by max_auxiliary_calls,
parent tool calls, duration and token budgets. Every request atomically reserves estimated
input plus the model's output_reservation before external execution. Known model windows are
enforced. The auxiliary_json_v1 count identifies estimated native JSON framing and per-image
fallback, not exact service tokenization or a hard billed-cost cap.

A durable Started extension record precedes HTTP. Failed/invalid/cancelled requests still
settle usage: service counts are reported before answer validation; missing/interrupted usage
charges the reserved estimate with explicit uncertainty. Reservations release on all outcomes,
including dropped futures, without double charging. Native HTTP makes no implicit retry or
redirect; completed inference/delivery failure cannot repeat the operation. Unsupported
journals fail as persistence errors before provider IO. A process loss/cancel can leave a
Started record without Finished; final statistics retain known interruption accounting when
available. Such history never authorizes replay or silently reruns a task.

RunProgress.usage and terminal usage include auxiliary tokens. primary_usage retains the main/
summary subtotal when tasks exist, and auxiliary carries per-attempt provider/model, status,
precision and delivery completeness. Session TurnStatistics, terminal event summaries and RPC
results preserve the breakdown. Tool results carry their own usage. Native request records
use aicoder.auxiliary.v1 and do not enter model instructions or change transcript indices.
Cost remains unknown without an applicable auxiliary price: TUI does not apply the primary
model's rates to mixed usage or turn missing prices into zero. Service counts are not an invoice.

See [acceptance](s7-classifiers-acceptance.md), [Code Mode](code-mode.md) and [migration](migrations.md).
