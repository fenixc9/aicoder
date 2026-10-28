use aicoder_core::model::*;
use serde_json::json;

fn request() -> ModelRequest {
    ModelRequest::new("test", vec![Message::text(MessageRole::User, "hello")])
}

#[test]
fn content_blocks_round_trip_without_treating_reasoning_or_state_as_text() {
    let message = Message {
        role: MessageRole::Assistant,
        content: vec![
            ContentBlock::Reasoning {
                text: "analysis".into(),
            },
            ContentBlock::Text {
                text: "hello".into(),
            },
            ContentBlock::ProviderData {
                provider: "custom".into(),
                data: json!({"signature":"opaque"}),
            },
            ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "call".into(),
                    name: "read".into(),
                    arguments: json!({"path":"a.rs"}),
                },
            },
            ContentBlock::Text {
                text: " world".into(),
            },
        ],
    };
    assert_eq!(message.text_content(), "hello world");
    assert_eq!(
        message.tool_calls().next().unwrap().arguments["path"],
        "a.rs"
    );
    assert_eq!(
        serde_json::from_str::<Message>(&serde_json::to_string(&message).unwrap()).unwrap(),
        message
    );
}

#[test]
fn capabilities_reject_unsupported_input_instead_of_silently_discarding_it() {
    let mut spec = ModelSpec::new("custom", "test");
    let mut input = request();
    input.options.response_format = Some(ResponseFormat::Text);
    input.validate(&spec).unwrap();
    input.messages[0].content.push(ContentBlock::Image {
        source: ImageSource::Url {
            url: "https://example.com/image.png".into(),
        },
    });
    assert_eq!(
        input.validate(&spec).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
    spec.capabilities.images = true;
    input.validate(&spec).unwrap();
    input.options.temperature = Some(0.0);
    assert_eq!(
        input.validate(&spec).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
    spec.capabilities.temperature = true;
    input.validate(&spec).unwrap();
    input.options.temperature = Some(f32::NAN);
    assert_eq!(
        input.validate(&spec).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
}

#[test]
fn foreign_provider_state_cannot_be_replayed() {
    let spec = ModelSpec::new("one", "test");
    let mut input = request();
    input.messages.push(Message {
        role: MessageRole::Assistant,
        content: vec![ContentBlock::ProviderData {
            provider: "two".into(),
            data: json!({"signature":"private"}),
        }],
    });
    assert_eq!(
        input.validate(&spec).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
}

#[test]
fn tools_and_limits_have_protocol_independent_validation() {
    let mut spec = ModelSpec::new("custom", "test");
    spec.capabilities.tools = true;
    spec.max_output_tokens = Some(100);
    let mut input = request();
    input.tools.push(ToolDefinition {
        name: "read".into(),
        description: None,
        parameters: json!({"type":"object"}),
    });
    input.tool_choice = ModelToolChoice::Named("missing".into());
    assert_eq!(
        input.validate(&spec).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
    input.tool_choice = ModelToolChoice::Named("read".into());
    input.validate(&spec).unwrap();
    input.options.max_output_tokens = Some(101);
    assert_eq!(
        input.validate(&spec).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
    input.options.max_output_tokens = Some(100);
    input.tools.push(input.tools[0].clone());
    assert_eq!(
        input.validate(&spec).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
}

#[test]
fn unknown_usage_and_overflow_never_become_exact_zero() {
    let measured = ModelUsage {
        input_tokens: Some(4),
        output_tokens: Some(2),
        total_tokens: Some(6),
        ..Default::default()
    };
    let mut total = ModelUsage::zero();
    total.accumulate(&measured);
    assert_eq!(total.total_tokens, Some(6));
    assert_eq!(total.cached_input_tokens, None);
    total.accumulate(&ModelUsage::default());
    assert_eq!(total.total_tokens, None);
    total.accumulate(&measured);
    assert_eq!(total.total_tokens, None);
    let mut overflow = ModelUsage {
        total_tokens: Some(u64::MAX),
        ..Default::default()
    };
    overflow.accumulate(&measured);
    assert_eq!(overflow.total_tokens, None);
}

#[test]
fn model_registry_is_scoped_by_provider_and_rejects_duplicate_identity() {
    let mut registry = ModelRegistry::default();
    registry.register(ModelSpec::new("one", "test")).unwrap();
    registry.register(ModelSpec::new("two", "test")).unwrap();
    assert_eq!(registry.models().count(), 2);
    assert!(registry.get("one", "test").is_some());
    assert!(registry.get("three", "test").is_none());
    assert!(registry.register(ModelSpec::new("one", "test")).is_err());
    assert!(registry.register(ModelSpec::new("", "test")).is_err());
}

#[test]
fn provider_errors_are_typed_and_redacted() {
    let error = ProviderError::new(
        "custom",
        ProviderErrorKind::Authentication,
        "Authorization: Bearer private-secret api_key=another-secret",
    );
    assert!(!error.to_string().contains("private-secret"));
    assert!(!format!("{error:?}").contains("another-secret"));
    assert_eq!(error.kind, ProviderErrorKind::Authentication);
    assert!(!error.retryable);
}

#[test]
fn tool_history_requires_complete_matching_call_result_units() {
    let mut spec = ModelSpec::new("custom", "test");
    spec.capabilities.tools = true;
    let mut input = request();
    input.messages.push(Message {
        role: MessageRole::Assistant,
        content: vec![ContentBlock::ToolCall {
            call: ModelToolCall {
                id: "c".into(),
                name: "read".into(),
                arguments: json!({}),
            },
        }],
    });
    assert_eq!(
        input.validate(&spec).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
    input.messages.push(Message {
        role: MessageRole::Tool,
        content: vec![ContentBlock::ToolResult {
            call_id: "wrong".into(),
            content: json!("text"),
            is_error: false,
        }],
    });
    assert_eq!(
        input.validate(&spec).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
    input.messages.last_mut().unwrap().content = vec![ContentBlock::ToolResult {
        call_id: "c".into(),
        content: json!("text"),
        is_error: false,
    }];
    input.validate(&spec).unwrap();
}
