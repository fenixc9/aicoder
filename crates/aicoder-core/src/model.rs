//! Provider-neutral model contract. Transport payloads belong to adapter crates.
use std::{collections::BTreeMap, error::Error, fmt, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{TurnExecutionContext, redaction::sanitize_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// Concatenates text blocks in order; reasoning and tool payloads are not visible text.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ModelToolCall> {
        self.content.iter().filter_map(|block| match block {
            ContentBlock::ToolCall { call } => Some(call),
            _ => None,
        })
    }

    /// Returns the first tool result's identity, output and error flag.
    pub fn tool_result(&self) -> Option<(&str, &Value, bool)> {
        self.content.iter().find_map(|block| match block {
            ContentBlock::ToolResult {
                call_id,
                content,
                is_error,
            } => Some((call_id.as_str(), content, *is_error)),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    ToolCall {
        call: ModelToolCall,
    },
    ToolResult {
        call_id: String,
        content: Value,
        is_error: bool,
    },
    /// Opaque state can only be replayed by an adapter that understands this provider's format.
    ProviderData {
        provider: String,
        data: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Url { url: String },
    Base64 { media_type: String, data: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: Option<String>,
    pub parameters: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "name", rename_all = "snake_case")]
pub enum ModelToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: Value,
        strict: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelOptions {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub seed: Option<i64>,
    pub stop: Option<Vec<String>>,
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub tool_choice: ModelToolChoice,
    #[serde(default)]
    pub options: ModelOptions,
    #[serde(default)]
    pub stream: bool,
}

impl ModelRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            tool_choice: ModelToolChoice::Auto,
            options: ModelOptions::default(),
            stream: true,
        }
    }

    pub fn validate(&self, spec: &ModelSpec) -> Result<(), ProviderError> {
        spec.validate()?;
        let invalid = |message: &str| {
            ProviderError::new(&spec.provider, ProviderErrorKind::InvalidRequest, message)
        };
        let unsupported = |feature: &str| {
            ProviderError::new(
                &spec.provider,
                ProviderErrorKind::Unsupported,
                format!("Model {} does not support {feature}", spec.id),
            )
        };
        if self.model != spec.id {
            return Err(invalid("request model does not match model specification"));
        }
        if self.messages.is_empty() {
            return Err(invalid("messages must not be empty"));
        }
        let caps = &spec.capabilities;
        for (requested, supported, name) in [
            (
                !self.tools.is_empty()
                    || matches!(
                        self.tool_choice,
                        ModelToolChoice::Required | ModelToolChoice::Named(_)
                    ),
                caps.tools,
                "tools",
            ),
            (
                self.options.temperature.is_some(),
                caps.temperature,
                "temperature",
            ),
            (self.options.top_p.is_some(), caps.top_p, "top_p"),
            (self.options.seed.is_some(), caps.seed, "seed"),
            (
                self.options.stop.is_some(),
                caps.stop_sequences,
                "stop sequences",
            ),
            (
                !matches!(
                    &self.options.response_format,
                    None | Some(ResponseFormat::Text)
                ),
                caps.structured_output,
                "response format",
            ),
        ] {
            if requested && !supported {
                return Err(unsupported(name));
            }
        }
        if self.options.max_output_tokens == Some(0) {
            return Err(invalid("max_output_tokens must be positive"));
        }
        if let (Some(requested), Some(maximum)) =
            (self.options.max_output_tokens, spec.max_output_tokens)
            && requested > maximum
        {
            return Err(invalid("max_output_tokens exceeds model limit"));
        }
        if self
            .options
            .temperature
            .is_some_and(|v| !v.is_finite() || v < 0.0)
        {
            return Err(invalid("temperature must be finite and nonnegative"));
        }
        if self
            .options
            .top_p
            .is_some_and(|v| !v.is_finite() || v <= 0.0 || v > 1.0)
        {
            return Err(invalid("top_p must be in (0, 1]"));
        }
        if let Some(ResponseFormat::JsonSchema { name, schema, .. }) = &self.options.response_format
            && (name.is_empty() || !schema.is_object())
        {
            return Err(invalid(
                "response schema needs a name and a JSON schema object",
            ));
        }
        let mut names = std::collections::HashSet::new();
        for tool in &self.tools {
            if tool.name.is_empty() || !names.insert(&tool.name) {
                return Err(invalid("tool names must be nonempty and unique"));
            }
            if !tool.parameters.is_object() {
                return Err(invalid("tool parameters must be a JSON schema object"));
            }
        }
        if let ModelToolChoice::Named(name) = &self.tool_choice
            && !names.contains(name)
        {
            return Err(invalid("named tool is not registered in this request"));
        }
        if matches!(self.tool_choice, ModelToolChoice::Required) && self.tools.is_empty() {
            return Err(invalid("required tool choice needs at least one tool"));
        }
        let mut pending_calls = std::collections::HashSet::new();
        for message in &self.messages {
            if message.content.is_empty() {
                return Err(invalid("message content must not be empty"));
            }
            if message.role != MessageRole::Tool && !pending_calls.is_empty() {
                return Err(invalid(
                    "tool calls must be followed by all matching results",
                ));
            }
            for block in &message.content {
                if message.role == MessageRole::Tool
                    && !matches!(block, ContentBlock::ToolResult { .. })
                {
                    return Err(invalid("tool messages must contain only tool results"));
                }
                match block {
                    ContentBlock::ToolCall { .. } | ContentBlock::ToolResult { .. }
                        if !caps.tools =>
                    {
                        return Err(unsupported("tool history"));
                    }
                    ContentBlock::Image { .. } if !caps.images => {
                        return Err(unsupported("image input"));
                    }
                    ContentBlock::Reasoning { .. } if !caps.reasoning_history => {
                        return Err(unsupported("reasoning history"));
                    }
                    ContentBlock::Reasoning { .. } if message.role != MessageRole::Assistant => {
                        return Err(invalid("reasoning history requires an assistant role"));
                    }
                    ContentBlock::ProviderData { provider, .. } if provider != &spec.provider => {
                        return Err(unsupported("foreign provider state"));
                    }
                    ContentBlock::ToolCall { call } => {
                        if message.role != MessageRole::Assistant
                            || call.id.is_empty()
                            || call.name.is_empty()
                            || !call.arguments.is_object()
                        {
                            return Err(invalid(
                                "tool calls require an assistant role, ID, name and JSON object arguments",
                            ));
                        }
                        if !pending_calls.insert(call.id.as_str()) {
                            return Err(invalid("duplicate pending tool call ID"));
                        }
                    }
                    ContentBlock::ToolResult { call_id, .. }
                        if message.role != MessageRole::Tool || call_id.is_empty() =>
                    {
                        return Err(invalid(
                            "tool results require a tool role and nonempty call ID",
                        ));
                    }
                    ContentBlock::ToolResult { call_id, .. } => {
                        if !pending_calls.remove(call_id.as_str()) {
                            return Err(invalid("tool result has no matching pending call"));
                        }
                    }
                    _ => {}
                }
            }
        }
        if !pending_calls.is_empty() {
            return Err(invalid("request contains unresolved tool calls"));
        }
        Ok(())
    }
}

/// Optional counts preserve unknown measurements. Aggregation starts with `zero`, not `default`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

impl ModelUsage {
    pub fn zero() -> Self {
        Self {
            input_tokens: Some(0),
            output_tokens: Some(0),
            total_tokens: Some(0),
            cached_input_tokens: Some(0),
            cache_write_tokens: Some(0),
        }
    }

    /// A missing measurement or overflow makes that aggregate unknown, never an exact zero.
    pub fn accumulate(&mut self, other: &Self) {
        fn add(a: &mut Option<u64>, b: Option<u64>) {
            *a = a.and_then(|a| b.and_then(|b| a.checked_add(b)));
        }
        add(&mut self.input_tokens, other.input_tokens);
        add(&mut self.output_tokens, other.output_tokens);
        add(&mut self.total_tokens, other.total_tokens);
        add(&mut self.cached_input_tokens, other.cached_input_tokens);
        add(&mut self.cache_write_tokens, other.cache_write_tokens);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", content = "value", rename_all = "snake_case")]
pub enum StopReason {
    Complete,
    ToolCalls,
    MaxTokens,
    Filtered,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub id: Option<String>,
    pub model: String,
    pub message: Message,
    pub stop_reason: StopReason,
    pub usage: ModelUsage,
}

impl ModelResponse {
    pub fn validate(&self) -> Result<(), ProviderError> {
        let invalid = |message: &str| {
            ProviderError::new("model", ProviderErrorKind::InvalidResponse, message)
        };
        if self.message.role != MessageRole::Assistant {
            return Err(invalid("model response must be an assistant message"));
        }
        let mut ids = std::collections::HashSet::new();
        for call in self.message.tool_calls() {
            if call.id.is_empty()
                || call.name.is_empty()
                || !call.arguments.is_object()
                || !ids.insert(&call.id)
            {
                return Err(invalid("invalid or duplicate tool call"));
            }
        }
        if (self.stop_reason == StopReason::ToolCalls) == ids.is_empty() {
            return Err(invalid("stop reason and tool calls disagree"));
        }
        if self.stop_reason == StopReason::Complete
            && !self.message.content.iter().any(|block| {
                matches!(block, ContentBlock::Text {text} if !text.trim().is_empty())
                    || matches!(block, ContentBlock::Image { .. })
            })
        {
            return Err(invalid("empty completed response"));
        }
        Ok(())
    }
}

/// Emits a complete response for providers that do not produce incremental events.
pub fn emit_response(response: &ModelResponse, sink: &dyn ModelEventSink) {
    sink.emit(ModelEvent::Started);
    let mut index = 0;
    for block in &response.message.content {
        match block {
            ContentBlock::Text { text } => sink.emit(ModelEvent::TextDelta {
                delta: text.clone(),
            }),
            ContentBlock::Reasoning { text } => sink.emit(ModelEvent::ReasoningDelta {
                delta: text.clone(),
            }),
            ContentBlock::ToolCall { call } => {
                sink.emit(ModelEvent::ToolCallCompleted {
                    index,
                    call: call.clone(),
                });
                index += 1;
            }
            _ => {}
        }
    }
    sink.emit(ModelEvent::Usage {
        usage: response.usage.clone(),
    });
    sink.emit(ModelEvent::Finished {
        stop_reason: response.stop_reason.clone(),
    });
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCapabilities {
    pub tools: bool,
    pub images: bool,
    pub reasoning_history: bool,
    pub temperature: bool,
    pub top_p: bool,
    pub seed: bool,
    pub stop_sequences: bool,
    pub structured_output: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub currency: String,
    pub input_per_million: f64,
    pub output_per_million: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub provider: String,
    pub id: String,
    pub context_tokens: Option<u64>,
    pub max_output_tokens: Option<u32>,
    pub capabilities: ModelCapabilities,
    pub pricing: Option<ModelPricing>,
    /// Optional tokenizer identifier. No tokenizer dependency is required by this contract.
    pub tokenizer: Option<String>,
}

impl ModelSpec {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
            context_tokens: None,
            max_output_tokens: None,
            capabilities: ModelCapabilities::default(),
            pricing: None,
            tokenizer: None,
        }
    }

    pub fn validate(&self) -> Result<(), ProviderError> {
        let invalid = |message: &str| {
            ProviderError::new(&self.provider, ProviderErrorKind::InvalidRequest, message)
        };
        if self.provider.is_empty() || self.id.is_empty() {
            return Err(invalid("model identity must be nonempty"));
        }
        if self.context_tokens == Some(0) || self.max_output_tokens == Some(0) {
            return Err(invalid("model token limits must be positive"));
        }
        if let (Some(context), Some(output)) = (self.context_tokens, self.max_output_tokens)
            && u64::from(output) > context
        {
            return Err(invalid("model output limit exceeds context window"));
        }
        if let Some(pricing) = &self.pricing
            && (pricing.currency.is_empty()
                || !pricing.input_per_million.is_finite()
                || pricing.input_per_million < 0.0
                || !pricing.output_per_million.is_finite()
                || pricing.output_per_million < 0.0)
        {
            return Err(invalid(
                "model pricing must use finite nonnegative values and a currency",
            ));
        }
        Ok(())
    }
}

/// Metadata registration is independent of execution, HTTP clients, and global configuration.
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    models: BTreeMap<(String, String), ModelSpec>,
}

impl ModelRegistry {
    pub fn register(&mut self, spec: ModelSpec) -> Result<(), ProviderError> {
        spec.validate()?;
        let key = (spec.provider.clone(), spec.id.clone());
        if spec.provider.is_empty() || spec.id.is_empty() || self.models.contains_key(&key) {
            return Err(ProviderError::new(
                &spec.provider,
                ProviderErrorKind::InvalidRequest,
                "model identity must be nonempty and unique",
            ));
        }
        self.models.insert(key, spec);
        Ok(())
    }
    pub fn get(&self, provider: &str, model: &str) -> Option<&ModelSpec> {
        self.models.get(&(provider.to_string(), model.to_string()))
    }
    pub fn models(&self) -> impl Iterator<Item = &ModelSpec> {
        self.models.values()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    Authentication,
    RateLimited,
    Transport,
    InvalidRequest,
    Unsupported,
    ModelNotFound,
    InvalidResponse,
    Cancelled,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderError {
    pub provider: String,
    pub kind: ProviderErrorKind,
    pub message: String,
    pub retryable: bool,
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter: Option<String>,
}

impl ProviderError {
    pub fn new(
        provider: impl Into<String>,
        kind: ProviderErrorKind,
        message: impl AsRef<str>,
    ) -> Self {
        Self {
            provider: sanitize_text(&provider.into(), 256),
            kind,
            message: sanitize_text(message.as_ref(), 4096),
            retryable: false,
            http_status: None,
            code: None,
            parameter: None,
        }
    }
}
impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl Error for ProviderError {}

/// Provider events are independent of runs, sessions, transports and UI frameworks.
/// Sinks must return promptly; hosts may forward them to their own bounded queues.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelEvent {
    Started,
    TextDelta {
        delta: String,
    },
    ReasoningDelta {
        delta: String,
    },
    ToolCallDelta {
        index: u32,
        id_delta: Option<String>,
        name_delta: Option<String>,
        arguments_delta: Option<String>,
    },
    ToolCallCompleted {
        index: u32,
        call: ModelToolCall,
    },
    Usage {
        usage: ModelUsage,
    },
    Retry {
        attempt: u32,
        delay_ms: u64,
        reason: String,
    },
    Finished {
        stop_reason: StopReason,
    },
    Aborted {
        message: String,
    },
}

pub trait ModelEventSink: Send + Sync {
    fn emit(&self, event: ModelEvent);
}
impl<F: Fn(ModelEvent) + Send + Sync> ModelEventSink for F {
    fn emit(&self, event: ModelEvent) {
        self(event);
    }
}
impl ModelEventSink for () {
    fn emit(&self, _: ModelEvent) {}
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Pure local validation, including adapter-specific replay/option restrictions.
    /// Hosts may call this before persisting a model selection or starting a request.
    fn validate_request(&self, request: &ModelRequest) -> Result<(), ProviderError> {
        request.validate(&self.model_spec(&request.model)?)
    }

    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError>;

    /// Returns one assistant response. `stream` controls incremental events, not the result shape.
    /// Implementations validate capabilities and observe cancellation through the supplied context.
    async fn generate(
        &self,
        request: ModelRequest,
        events: Arc<dyn ModelEventSink>,
        context: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError>;
}
