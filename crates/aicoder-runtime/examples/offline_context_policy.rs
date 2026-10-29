//! Custom context policy with explicit count provenance. No network/model credentials.
use aicoder_core::{context::*, context_policy::*, model::*, tokens::*, *};
use aicoder_runtime::{TokenizerMode, tokenizers};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::json;
use std::{
    fs,
    sync::{Arc, Mutex},
};
struct Headroom;
impl ContextPolicy for Headroom {
    fn name(&self) -> &'static str {
        "example_headroom"
    }
    fn decide(&self, input: &ContextPolicyInput<'_>) -> Result<ContextDecision> {
        Ok(if input.count.tokens > 200 {
            ContextDecision::Compact { target_tokens: 200 }
        } else {
            ContextDecision::Keep
        })
    }
}
struct Offline(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl ModelProvider for Offline {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.context_tokens = Some(16384);
        spec.max_output_tokens = Some(128);
        spec.tokenizer = Some("cl100k_base".into());
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        assert_eq!(request.messages[0].text_content(), "trusted");
        assert_eq!(request.messages.last().unwrap().text_content(), "latest");
        assert!(
            !request
                .messages
                .iter()
                .any(|message| message.text_content().contains("OLD_HISTORY"))
        );
        let model = request.model.clone();
        self.0.lock().unwrap().push(request);
        let response = ModelResponse {
            id: None,
            model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mode: TokenizerMode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "heuristic".into())
        .parse()?;
    ensure!(
        cfg!(feature = "tokenizer-tiktoken")
            || matches!(mode, TokenizerMode::Heuristic | TokenizerMode::Auto),
        "Enable tokenizer-tiktoken for explicit BPE encodings"
    );
    let root = tempfile::tempdir()?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Offline(requests.clone());
    let spec = provider.model_spec("offline")?;
    let counter = tokenizers::counter(mode);
    let text = TokenCounting::new(counter.clone()).text(&spec, "hello world");
    let executor = TurnExecutor::builder(provider)
        .workspace(root.path())
        .token_counter(counter)
        .context_policy(Arc::new(Headroom))
        .context_compactor(PruningContextCompactor::new(ContextWindowConfig {
            max_context_tokens: 4096,
            reserved_output_tokens: 128,
            preserve_recent_tokens: 20,
        })?)
        .build()?;
    let original = vec![
        Message::text(MessageRole::System, "trusted"),
        Message::text(MessageRole::User, "OLD_HISTORY ".repeat(400)),
        Message::text(MessageRole::User, "latest"),
    ];
    let frames = Arc::new(Mutex::new(Vec::new()));
    let captured = frames.clone();
    let result = executor
        .run_outcome(
            ModelRequest::new("offline", original.clone()),
            Arc::new(move |frame: &AgentRawEventEnvelope| {
                captured.lock().unwrap().push(frame.clone())
            }),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    ensure!(result.progress.messages[..original.len()] == original);
    let info = result.progress.model_context.as_ref().unwrap();
    ensure!(info.compactions == 1 && info.last_input_count.as_ref().unwrap().tokens <= 200);
    let frames = frames.lock().unwrap();
    let mut validator = events::EventTraceValidator::new();
    for frame in frames.iter() {
        validator.observe(frame)?;
    }
    validator.finish()?;
    if let Some(path) = std::env::args().nth(2) {
        fs::write(
            path,
            frames
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()?
                .join("\n")
                + "\n",
        )?;
    }
    println!(
        "{}",
        json!({"status":"completed","mode":mode,"text_count":text,"context":info,"canonical_history_kept":true,"policy_target":200,"provider_messages":requests.lock().unwrap()[0].messages.len(),"accounting":result.progress.tokens,"event_frames":frames.len()})
    );
    Ok(())
}
