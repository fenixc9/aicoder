use aicoder_core::{context::*, context_policy::*, model::*, tokens::*, *};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};
fn input<'a>(model: &'a str, messages: &'a [Message]) -> TokenInput<'a> {
    TokenInput {
        provider: "fixture",
        model,
        tokenizer: None,
        content: TokenContent::Request {
            messages,
            tools: None,
            response_format: None,
            tool_choice: None,
        },
    }
}
#[test]
fn heuristic_preserves_legacy_counts_and_includes_response_constraints() {
    let messages = vec![
        Message::text(MessageRole::User, "你好\nhello"),
        Message::text(MessageRole::Assistant, "done"),
    ];
    let count = TokenCounting::default().count(input("fixture", &messages));
    assert_eq!(
        count.tokens,
        estimate_context_tokens(&messages, None) as u64
    );
    assert_eq!(count.accuracy, TokenAccuracy::Estimated);
    assert_eq!(count.method, "heuristic_v1");
    let mut request = ModelRequest::new("fixture", messages);
    let spec = ModelSpec::new("fixture", "fixture");
    let plain = TokenCounting::default().request(&request, &spec);
    request.options.response_format = Some(ResponseFormat::JsonSchema {
        name: "long schema".into(),
        schema: json!({"type":"object","properties":{"explanation":{"type":"string"}}}),
        strict: true,
    });
    assert!(TokenCounting::default().request(&request, &spec).tokens > plain.tokens);
}
struct BadCounter(u8);
impl TokenCounter for BadCounter {
    fn count(&self, input: TokenInput<'_>) -> Result<TokenCount, TokenFallback> {
        if self.0 == 0 {
            return Err(TokenFallback::CounterFailed);
        }
        let mut count = TokenCount::estimated(
            if self.0 == 1 { 0 } else { 10 },
            input.content.scope(),
            "custom",
        );
        if self.0 == 2 {
            count.method = "bad\nmethod".into();
        }
        if self.0 == 3 {
            count.accuracy = TokenAccuracy::Exact;
            count.fallback = Some(TokenFallback::MissingTokenizer);
        }
        if self.0 == 4 {
            count.scope = TokenScope::Output;
        }
        Ok(count)
    }
}
#[test]
fn failed_zero_malformed_and_contradictory_counts_fall_back_explicitly() {
    let messages = vec![Message::text(MessageRole::User, "not empty")];
    for case in 0..5 {
        let count =
            TokenCounting::new(Arc::new(BadCounter(case))).count(input("fixture", &messages));
        assert!(count.tokens > 0);
        assert_eq!(count.method, "heuristic_v1");
        assert_eq!(count.accuracy, TokenAccuracy::Estimated);
        assert!(count.fallback.is_some());
    }
    let usage = ModelUsage {
        input_tokens: Some(10),
        output_tokens: Some(2),
        total_tokens: Some(3),
        ..Default::default()
    };
    let accounted = run::TokenAccounting::from_usage(&usage, 1);
    assert_eq!(accounted.used, 12);
    assert!(accounted.estimated);
    let usage = ModelUsage {
        input_tokens: Some(50),
        ..Default::default()
    };
    let accounted = run::TokenAccounting::from_usage(&usage, 1);
    assert_eq!(accounted.used, 50);
    assert!(accounted.estimated);
}
struct Exact;
impl TokenCounter for Exact {
    fn count(&self, input: TokenInput<'_>) -> Result<TokenCount, TokenFallback> {
        Ok(TokenCount {
            tokens: match input.content {
                TokenContent::Request { .. } => 20,
                TokenContent::Output(_) => 5,
                TokenContent::Text(text) => {
                    if text.is_empty() {
                        0
                    } else {
                        5
                    }
                }
            },
            scope: input.content.scope(),
            accuracy: TokenAccuracy::Exact,
            method: "fixture_calibrated".into(),
            fallback: None,
        })
    }
}
struct Provider {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    usage: ModelUsage,
    window: Option<u64>,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.max_output_tokens = Some(1000);
        spec.context_tokens = self.window;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let model = request.model.clone();
        self.requests.lock().unwrap().push(request);
        let response = ModelResponse {
            id: None,
            model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: self.usage.clone(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::test]
async fn injected_count_drives_budget_clamping_and_request_provenance() {
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor = TurnExecutor::builder(Provider {
        requests: requests.clone(),
        usage: ModelUsage::default(),
        window: None,
    })
    .workspace(root.path())
    .token_counter(Arc::new(Exact))
    .config(TurnExecutionConfig {
        budget: RunBudget {
            max_tokens: Some(100),
            ..Default::default()
        },
        ..Default::default()
    })
    .build()
    .unwrap();
    let result = executor
        .run(ModelRequest::new(
            "fixture",
            vec![Message::text(MessageRole::User, "x".repeat(1000))],
        ))
        .await
        .unwrap();
    assert_eq!(
        requests.lock().unwrap()[0].options.max_output_tokens,
        Some(80)
    );
    assert_eq!(result.progress.tokens.used, 25);
    assert!(result.progress.tokens.estimated);
    let count = result
        .progress
        .model_context
        .as_ref()
        .unwrap()
        .last_input_count
        .as_ref()
        .unwrap();
    assert_eq!(count.tokens, 20);
    assert_eq!(count.accuracy, TokenAccuracy::Exact);
    assert_eq!(count.method, "fixture_calibrated");
}
#[tokio::test]
async fn reported_usage_owns_accounting_even_when_context_count_is_estimated() {
    let root = tempfile::tempdir().unwrap();
    let usage = ModelUsage {
        input_tokens: Some(3),
        output_tokens: Some(4),
        total_tokens: Some(7),
        ..Default::default()
    };
    let executor = TurnExecutor::builder(Provider {
        requests: Arc::new(Mutex::new(Vec::new())),
        usage,
        window: None,
    })
    .workspace(root.path())
    .build()
    .unwrap();
    let result = executor
        .run(ModelRequest::new(
            "fixture",
            vec![Message::text(MessageRole::User, "hello")],
        ))
        .await
        .unwrap();
    assert_eq!(result.progress.tokens.used, 7);
    assert!(!result.progress.tokens.estimated);
    assert_eq!(
        result
            .progress
            .model_context
            .unwrap()
            .last_input_count
            .unwrap()
            .accuracy,
        TokenAccuracy::Estimated
    );
}
struct Reject;
impl ContextPolicy for Reject {
    fn name(&self) -> &'static str {
        "fixture_reject"
    }
    fn decide(&self, _: &ContextPolicyInput<'_>) -> anyhow::Result<ContextDecision> {
        Ok(ContextDecision::Reject {
            code: "case_limit".into(),
        })
    }
}
#[tokio::test]
async fn policy_rejection_closes_a_valid_trace_without_model_or_history_rewrites() {
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor = TurnExecutor::builder(Provider {
        requests: requests.clone(),
        usage: ModelUsage::zero(),
        window: None,
    })
    .workspace(root.path())
    .context_policy(Arc::new(Reject))
    .build()
    .unwrap();
    let frames = Arc::new(Mutex::new(Vec::new()));
    let captured = frames.clone();
    let messages = vec![Message::text(MessageRole::User, "original")];
    let outcome = executor
        .run_outcome(
            ModelRequest::new("fixture", messages.clone()),
            Arc::new(move |frame: &AgentRawEventEnvelope| {
                captured.lock().unwrap().push(frame.clone())
            }),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(outcome.stop().is_some());
    assert!(requests.lock().unwrap().is_empty());
    assert_eq!(outcome.progress().messages, messages);
    let mut validator = events::EventTraceValidator::new();
    for frame in frames.lock().unwrap().iter() {
        validator.observe(frame).unwrap();
    }
    validator.finish().unwrap();
    let info = outcome.progress().model_context.as_ref().unwrap();
    assert_eq!(info.context_policy.as_deref(), Some("fixture_reject"));
    assert!(info.context_count.is_some());
}
struct Shrink;
impl ContextPolicy for Shrink {
    fn name(&self) -> &'static str {
        "fixture_shrink"
    }
    fn decide(&self, input: &ContextPolicyInput<'_>) -> anyhow::Result<ContextDecision> {
        Ok(if input.count.tokens > 100 {
            ContextDecision::Compact { target_tokens: 100 }
        } else {
            ContextDecision::Keep
        })
    }
}
#[tokio::test]
async fn custom_policy_target_is_honored_by_counted_pruning() {
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let compactor = PruningContextCompactor::new(ContextWindowConfig {
        max_context_tokens: 1000,
        reserved_output_tokens: 100,
        preserve_recent_tokens: 10,
    })
    .unwrap();
    let executor = TurnExecutor::builder(Provider {
        requests: requests.clone(),
        usage: ModelUsage::zero(),
        window: None,
    })
    .workspace(root.path())
    .context_compactor(compactor)
    .context_policy(Arc::new(Shrink))
    .build()
    .unwrap();
    let messages = vec![
        Message::text(MessageRole::System, "prefix"),
        Message::text(MessageRole::User, "old ".repeat(200)),
        Message::text(MessageRole::User, "latest"),
    ];
    let result = executor
        .run(ModelRequest::new("fixture", messages.clone()))
        .await
        .unwrap();
    assert_eq!(result.progress.messages[..messages.len()], messages);
    let sent = requests.lock().unwrap();
    assert_eq!(sent[0].messages[0].text_content(), "prefix");
    assert_eq!(sent[0].messages.last().unwrap().text_content(), "latest");
    assert!(estimate_context_tokens(&sent[0].messages, Some(&[])) <= 100);
}
struct Keep;
impl ContextPolicy for Keep {
    fn name(&self) -> &'static str {
        "fixture_keep"
    }
    fn decide(&self, _: &ContextPolicyInput<'_>) -> anyhow::Result<ContextDecision> {
        Ok(ContextDecision::Keep)
    }
}
#[tokio::test]
async fn custom_policy_cannot_bypass_known_model_capacity() {
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor = TurnExecutor::builder(Provider {
        requests: requests.clone(),
        usage: ModelUsage::zero(),
        window: Some(1100),
    })
    .workspace(root.path())
    .context_policy(Arc::new(Keep))
    .build()
    .unwrap();
    let mut request = ModelRequest::new(
        "fixture",
        vec![Message::text(MessageRole::User, "x".repeat(800))],
    );
    request.options.max_output_tokens = Some(1000);
    assert!(executor.run(request).await.is_err());
    assert!(requests.lock().unwrap().is_empty());
}
struct DropPrefix;
#[async_trait]
impl ContextCompactor for DropPrefix {
    fn name(&self) -> &'static str {
        "bad_legacy"
    }
    fn estimate_tokens(&self, _: ContextCompactionInput<'_>) -> usize {
        1000
    }
    fn target_tokens(&self, _: ContextCompactionInput<'_>) -> usize {
        100
    }
    async fn compact(&self, _: ContextCompactionInput<'_>) -> anyhow::Result<ContextCompaction> {
        Ok(ContextCompaction {
            messages: vec![Message::text(MessageRole::User, "short")],
            estimated_tokens_before: 1000,
            estimated_tokens_after: 10,
            removed_messages: 1,
            usage: ModelUsage {
                total_tokens: Some(7),
                ..Default::default()
            },
        })
    }
}
#[tokio::test]
async fn invalid_compactor_keeps_consumed_usage_but_never_drops_the_runtime_prefix() {
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor = TurnExecutor::builder(Provider {
        requests: requests.clone(),
        usage: ModelUsage::zero(),
        window: None,
    })
    .workspace(root.path())
    .context_compactor(DropPrefix)
    .build()
    .unwrap();
    let messages = vec![
        Message::text(MessageRole::System, "trusted"),
        Message::text(MessageRole::User, "old".repeat(1000)),
    ];
    let outcome = executor
        .run_outcome(
            ModelRequest::new("fixture", messages.clone()),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(outcome.stop().is_some());
    assert_eq!(outcome.progress().usage.total_tokens, Some(7));
    assert_eq!(outcome.progress().tokens.used, 7);
    assert_eq!(outcome.progress().messages, messages);
    assert!(requests.lock().unwrap().is_empty());
}

struct SummaryCounter(Arc<Mutex<Vec<String>>>);
impl TokenCounter for SummaryCounter {
    fn count(&self, input: TokenInput<'_>) -> Result<TokenCount, TokenFallback> {
        self.0.lock().unwrap().push(input.model.to_string());
        if input.model == "summary" && matches!(input.content, TokenContent::Request { .. }) {
            Ok(TokenCount {
                tokens: 20,
                scope: TokenScope::Request,
                accuracy: TokenAccuracy::Exact,
                method: "fixture_summary".into(),
                fallback: None,
            })
        } else {
            HeuristicTokenCounter.count(input)
        }
    }
}
struct SummaryProvider(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl ModelProvider for SummaryProvider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.context_tokens = Some(16384);
        spec.max_output_tokens = Some(128);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let summary = request.model == "summary";
        let first = if summary {
            self.0
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.model != "summary")
        } else {
            false
        };
        let model = request.model.clone();
        self.0.lock().unwrap().push(request);
        let text = if !summary {
            "done".into()
        } else if first {
            "invalid summary".into()
        } else {
            json!({"goals":["finish"],"constraints":["keep"],"changes":[],"todos":["test"],"notes":[]}).to_string()
        };
        let response = ModelResponse {
            id: None,
            model,
            message: Message::text(MessageRole::Assistant, text),
            stop_reason: StopReason::Complete,
            usage: if summary {
                ModelUsage::default()
            } else {
                ModelUsage::zero()
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::test]
async fn summary_chunks_retries_and_usage_share_the_counter_for_the_summary_model() {
    use aicoder_core::summary::*;
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(SummaryProvider(requests.clone()));
    let models = Arc::new(Mutex::new(Vec::new()));
    let mut config = SummaryConfig::new(ContextWindowConfig {
        max_context_tokens: 1200,
        reserved_output_tokens: 200,
        preserve_recent_tokens: 80,
    });
    config.output_tokens = 128;
    config.max_input_tokens = 1400;
    config.max_calls = 2;
    config.failure_policy = SummaryFailurePolicy::Stop;
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .context_compactor(SemanticContextCompactor::new(provider, "summary", config).unwrap())
        .token_counter(Arc::new(SummaryCounter(models.clone())))
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_tokens: Some(500),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let mut messages = (0..8)
        .map(|_| Message::text(MessageRole::User, "x".repeat(1000)))
        .collect::<Vec<_>>();
    messages.push(Message::text(MessageRole::User, "latest"));
    let result = executor
        .run(ModelRequest::new("coding", messages))
        .await
        .unwrap();
    assert_eq!(result.progress.model_context.unwrap().summary_attempts, 2);
    assert_eq!(result.progress.tokens.used, 296);
    assert!(result.progress.tokens.estimated);
    assert!(
        models
            .lock()
            .unwrap()
            .iter()
            .any(|model| model == "summary")
    );
    let sent = requests.lock().unwrap();
    assert_eq!(
        sent.iter()
            .filter(|request| request.model == "summary")
            .count(),
        2
    );
    assert!(
        sent.iter()
            .filter(|request| request.model == "summary")
            .all(|request| request.options.max_output_tokens == Some(128))
    );
}

struct PerMessage;
impl TokenCounter for PerMessage {
    fn count(&self, input: TokenInput<'_>) -> Result<TokenCount, TokenFallback> {
        let tokens = match input.content {
            TokenContent::Request { messages, .. } => messages.len() as u64 * 20,
            TokenContent::Output(_) => 5,
            TokenContent::Text(text) => {
                if text.is_empty() {
                    0
                } else {
                    5
                }
            }
        };
        Ok(TokenCount {
            tokens,
            scope: input.content.scope(),
            accuracy: TokenAccuracy::Exact,
            method: "fixture_messages".into(),
            fallback: None,
        })
    }
}
#[tokio::test]
async fn nonadditive_pruning_recounts_candidates_without_splitting_tool_units() {
    let mut spec = ModelSpec::new("fixture", "fixture");
    spec.capabilities.tools = true;
    let messages = vec![
        Message::text(MessageRole::System, "prefix"),
        Message::text(MessageRole::User, "old one"),
        Message::text(MessageRole::User, "old two"),
        Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "last".into(),
                    name: "work".into(),
                    arguments: json!({}),
                },
            }],
        },
        Message {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id: "last".into(),
                content: json!({"ok":true}),
                is_error: false,
            }],
        },
    ];
    let compactor = PruningContextCompactor::new(ContextWindowConfig {
        max_context_tokens: 200,
        reserved_output_tokens: 20,
        preserve_recent_tokens: 10,
    })
    .unwrap();
    let result = compactor
        .compact_counted(
            summary::CompactionRequest {
                input: ContextCompactionInput {
                    model: "fixture",
                    messages: &messages,
                    tools: None,
                    max_output_tokens: None,
                },
                history: &messages,
                system_prefix_len: 1,
                force: false,
                token_budget: None,
            },
            ContextMeasure {
                counting: TokenCounting::new(Arc::new(PerMessage)),
                spec: spec.clone(),
                response_format: None,
                tool_choice: ModelToolChoice::Auto,
                target_tokens: 80,
            },
            TurnExecutionContext::new(),
        )
        .await
        .unwrap()
        .compaction;
    assert_eq!(result.estimated_tokens_after, 80);
    assert_eq!(result.messages[0], messages[0]);
    assert!(result.messages.ends_with(&messages[3..]));
    ModelRequest::new("fixture", result.messages)
        .validate(&spec)
        .unwrap();
}
