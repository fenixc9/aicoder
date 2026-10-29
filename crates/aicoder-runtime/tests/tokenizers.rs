use aicoder_core::{model::*, tokens::*, *};
use aicoder_runtime::{resources::ResourceSettings, *};
use async_trait::async_trait;
#[cfg(feature = "tokenizer-tiktoken")]
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
fn environment(root: &std::path::Path, mode: &str) -> Arc<dyn Environment> {
    Arc::new(BTreeMap::from([
        ("AICODER_HOME".into(), root.to_string_lossy().into_owned()),
        ("AICODER_TOKENIZER".into(), mode.into()),
    ]))
}
#[test]
fn tokenizer_settings_preserve_precedence_and_reject_unavailable_explicit_encodings() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                context: ContextSettings {
                    tokenizer: Setting::Value(TokenizerMode::Heuristic),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        environment(root.path(), "invalid-but-shadowed"),
    )
    .unwrap();
    assert_eq!(runtime.config().context.tokenizer, TokenizerMode::Heuristic);
    assert!(matches!(
        runtime.config().sources["context.tokenizer"],
        ConfigSource::Explicit
    ));
    assert!(
        Runtime::load_with_environment(
            workspace.path(),
            Default::default(),
            ConfigDiscovery {
                enabled: false,
                ..Default::default()
            },
            environment(root.path(), "invalid")
        )
        .is_err()
    );
    let result = Runtime::load_with_environment(
        workspace.path(),
        Default::default(),
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        environment(root.path(), "cl100k_base"),
    );
    #[cfg(feature = "tokenizer-tiktoken")]
    assert_eq!(
        result.unwrap().config().context.tokenizer,
        TokenizerMode::Cl100kBase
    );
    #[cfg(not(feature = "tokenizer-tiktoken"))]
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("tokenizer-tiktoken")
    );
}
struct Model;
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.tools = true;
        spec.tokenizer = Some("cl100k_base".into());
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::test]
async fn shared_runtime_auto_mode_reports_the_available_method_without_guessing_names() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        environment(root.path(), "auto"),
    )
    .unwrap();
    let executor = runtime
        .executor_builder(Arc::new(Model))
        .unwrap()
        .build()
        .unwrap();
    let result = executor
        .run(ModelRequest::new(
            "arbitrary-model-name",
            vec![Message::text(MessageRole::User, "你好世界")],
        ))
        .await
        .unwrap();
    let count = result
        .progress
        .model_context
        .unwrap()
        .last_input_count
        .unwrap();
    assert_eq!(count.accuracy, TokenAccuracy::Estimated);
    #[cfg(feature = "tokenizer-tiktoken")]
    {
        assert_eq!(count.method, "tokenizer:cl100k_base");
        assert!(count.fallback.is_none());
    }
    #[cfg(not(feature = "tokenizer-tiktoken"))]
    {
        assert_eq!(count.method, "heuristic_v1");
        assert_eq!(count.fallback, Some(TokenFallback::TokenizerUnavailable));
    }
    let counting = TokenCounting::new(tokenizers::counter(TokenizerMode::Auto));
    let unknown = counting.count(TokenInput {
        provider: "fixture",
        model: "gpt-whatever",
        tokenizer: None,
        content: TokenContent::Text("hello"),
    });
    assert_eq!(unknown.fallback, Some(TokenFallback::MissingTokenizer));
}
#[cfg(feature = "tokenizer-tiktoken")]
#[test]
fn bpe_counts_real_multilingual_encodings_and_treats_special_spellings_as_text() {
    let tokenizer = tokenizers::LocalTokenizer;
    for (encoding, text, expected) in [
        ("cl100k_base", "hello world", 2),
        ("o200k_base", "hello world", 2),
        ("cl100k_base", "お誕生日おめでとう", 9),
        ("o200k_base", "お誕生日おめでとう", 8),
        ("cl100k_base", "antidisestablishmentarianism", 6),
    ] {
        assert_eq!(tokenizer.count_text(text, encoding).unwrap(), expected);
    }
    assert!(
        tokenizer
            .count_text("<|endoftext|>", "cl100k_base")
            .unwrap()
            > 1
    );
    let counting = TokenCounting::new(tokenizers::counter(TokenizerMode::Cl100kBase));
    let count = counting.count(TokenInput {
        provider: "fixture",
        model: "fixture",
        tokenizer: None,
        content: TokenContent::Text("hello world"),
    });
    assert_eq!(count.tokens, 2);
    assert_eq!(count.accuracy, TokenAccuracy::Exact);
    assert_eq!(count.scope, TokenScope::Text);
    let source = vec![Message::text(MessageRole::User, "hello world")];
    let count = counting.count(TokenInput {
        provider: "fixture",
        model: "fixture",
        tokenizer: None,
        content: TokenContent::Request {
            messages: &source,
            tools: None,
            response_format: None,
            tool_choice: None,
        },
    });
    assert_eq!(count.accuracy, TokenAccuracy::Estimated);
    assert!(count.tokens > 2);
}
#[cfg(feature = "tokenizer-tiktoken")]
#[test]
fn large_or_nontext_inputs_fall_back_instead_of_claiming_exact_provider_tokens() {
    let counting = TokenCounting::new(tokenizers::counter(TokenizerMode::Cl100kBase));
    let spec = ModelSpec::new("fixture", "fixture");
    let large = counting.text(&spec, &"x".repeat(2 * 1024 * 1024 + 1));
    assert_eq!(large.accuracy, TokenAccuracy::Estimated);
    assert_eq!(large.fallback, Some(TokenFallback::InputLimit));
    let messages = vec![Message {
        role: MessageRole::User,
        content: vec![ContentBlock::ProviderData {
            provider: "fixture".into(),
            data: json!({"opaque":"state"}),
        }],
    }];
    let mut request = ModelRequest::new("fixture", messages);
    let count = counting.request(&request, &spec);
    assert_eq!(count.fallback, Some(TokenFallback::UnsupportedContent));
    request.messages = vec![Message::text(MessageRole::User, "tool schema")];
    request.tools = vec![ToolDefinition {
        name: "work".into(),
        description: None,
        parameters: json!({"type":"object"}),
    }];
    assert_eq!(
        counting.request(&request, &spec).accuracy,
        TokenAccuracy::Estimated
    );
}
