//! Conversion boundary for the provider-neutral API. No second execution loop lives here.
use crate::wire::{ChatCompletionResponse, FinishReason, ToolCall, Usage};
use std::sync::{Arc, Mutex};

use aicoder_core::{TurnCancelled, TurnExecutionContext, events::AgentRawEvent, model::*};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{ChatClient, ProviderEventSink};

const PROVIDER: &str = "openai-compatible";

fn error(kind: ProviderErrorKind, message: impl AsRef<str>) -> ProviderError {
    ProviderError::new(PROVIDER, kind, message)
}

impl ChatClient {
    /// Overrides model metadata or registers an additional model on this endpoint.
    /// Metadata is declarative, not an assertion that an arbitrary compatible server implements it.
    pub fn register_model(&mut self, spec: ModelSpec) -> Result<(), ProviderError> {
        if spec.provider != self.config.provider_id {
            return Err(ProviderError::new(
                &self.config.provider_id,
                ProviderErrorKind::InvalidRequest,
                "model belongs to another provider",
            ));
        }
        if spec.capabilities.reasoning_efforts.iter().any(|effort| {
            match self.config.compatibility {
                crate::CompatibilityProfile::Generic => false,
                crate::CompatibilityProfile::DeepseekThinking => *effort != ReasoningEffort::High,
                crate::CompatibilityProfile::DeepseekChat => *effort != ReasoningEffort::None,
            }
        }) {
            return Err(ProviderError::new(
                &self.config.provider_id,
                ProviderErrorKind::InvalidRequest,
                "reasoning metadata conflicts with the compatibility profile",
            ));
        }
        self.models.register(spec)
    }
}

#[async_trait]
impl ModelProvider for ChatClient {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        if let Some(spec) = self.models.get(&self.config.provider_id, model) {
            return Ok(spec.clone());
        }
        if model != self.config.model {
            return Err(ProviderError::new(
                &self.config.provider_id,
                ProviderErrorKind::ModelNotFound,
                format!("Model {model} is not registered"),
            ));
        }
        let mut spec = ModelSpec::new(&self.config.provider_id, model);
        // Common chat-completions protocol profile. Hosts can register a more restrictive spec.
        spec.capabilities = ModelCapabilities {
            tools: true,
            temperature: true,
            top_p: true,
            seed: true,
            stop_sequences: true,
            structured_output: true,
            ..Default::default()
        };
        match self.config.compatibility {
            crate::CompatibilityProfile::Generic => {}
            crate::CompatibilityProfile::DeepseekThinking => {
                spec.capabilities.reasoning_history = true;
                spec.capabilities.reasoning_efforts = vec![ReasoningEffort::High];
                spec.capabilities.temperature = false;
                spec.capabilities.top_p = false;
                spec.capabilities.seed = false;
            }
            crate::CompatibilityProfile::DeepseekChat => {
                spec.capabilities.reasoning_efforts = vec![ReasoningEffort::None];
                spec.capabilities.top_p = false;
                spec.capabilities.seed = false;
            }
        }
        Ok(spec)
    }

    fn validate_request(&self, request: &ModelRequest) -> Result<(), ProviderError> {
        request.validate(&self.model_spec(&request.model)?)?;
        let unsupported = |message: &str| {
            ProviderError::new(
                &self.config.provider_id,
                ProviderErrorKind::Unsupported,
                message,
            )
        };
        if self.config.compatibility == crate::CompatibilityProfile::DeepseekThinking
            && matches!(
                request.tool_choice,
                ModelToolChoice::Required | ModelToolChoice::Named(_)
            )
        {
            return Err(unsupported(
                "DeepSeek thinking mode does not support required or named tool choice",
            ));
        }
        if self.config.compatibility != crate::CompatibilityProfile::Generic
            && matches!(
                request.options.response_format,
                Some(ResponseFormat::JsonSchema { .. })
            )
        {
            return Err(unsupported(
                "DeepSeek compatibility profiles support JSON object output, not JSON Schema output",
            ));
        }
        if let Some(effort) = request.options.reasoning_effort
            && match self.config.compatibility {
                crate::CompatibilityProfile::Generic => false,
                crate::CompatibilityProfile::DeepseekThinking => effort != ReasoningEffort::High,
                crate::CompatibilityProfile::DeepseekChat => effort != ReasoningEffort::None,
            }
        {
            return Err(unsupported(
                "reasoning effort conflicts with compatibility profile",
            ));
        }
        for message in &request.messages {
            for block in &message.content {
                match block {
                    ContentBlock::ProviderData { .. } => {
                        return Err(unsupported(
                            "opaque provider state cannot be encoded by this adapter",
                        ));
                    }
                    ContentBlock::Image { source } => {
                        if !matches!(message.role, MessageRole::User | MessageRole::Tool) {
                            return Err(unsupported(
                                "this adapter accepts user images and anchored tool previews",
                            ));
                        }
                        let valid = match source {
                            ImageSource::Url { url } => {
                                url.starts_with("https://") || url.starts_with("http://")
                            }
                            ImageSource::Artifact { .. } => message.role == MessageRole::Tool,
                            ImageSource::Base64 { media_type, data } => {
                                aicoder_core::media::image_media_type(media_type).is_some()
                                    && !data.is_empty()
                            }
                        };
                        if !valid {
                            return Err(ProviderError::new(
                                &self.config.provider_id,
                                ProviderErrorKind::InvalidRequest,
                                "image source must be HTTP(S) or nonempty image base64",
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    async fn generate(
        &self,
        request: ModelRequest,
        events: Arc<dyn ModelEventSink>,
        context: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let mut result = async {
            if context.is_cancelled() {
                return Err(error(ProviderErrorKind::Cancelled, context.reason()));
            }
            self.validate_request(&request)?;
            let stream = request.stream;
            let modern_output = self.config.compatibility == crate::CompatibilityProfile::Generic
                && !self
                    .model_spec(&request.model)?
                    .capabilities
                    .reasoning_efforts
                    .is_empty();
            let mut body = encode_request(request)?;
            match self.config.compatibility {
                crate::CompatibilityProfile::Generic => {
                    if modern_output
                        && let Some(cap) = body.as_object_mut().unwrap().remove("max_tokens")
                    {
                        body["max_completion_tokens"] = cap;
                    }
                }
                crate::CompatibilityProfile::DeepseekThinking => {
                    body["thinking"] = json!({"type":"enabled"});
                    body["reasoning_effort"] = json!("high");
                    for message in body["messages"].as_array_mut().unwrap() {
                        if message["role"] == "assistant"
                            && message.get("reasoning_content").is_none()
                        {
                            message["reasoning_content"] = json!("");
                        }
                    }
                }
                crate::CompatibilityProfile::DeepseekChat => {
                    body.as_object_mut().unwrap().remove("reasoning_effort");
                    body["thinking"] = json!({"type":"disabled"})
                }
            }
            events.emit(ModelEvent::Started);
            let sink = NormalizedSink {
                events: events.clone(),
                usage: Mutex::new(None),
            };
            let raw = if stream {
                self.collect_stream_body(body, &sink, &context).await
            } else {
                tokio::select! {
                    biased;
                    cancelled = context.cancelled() => Err(cancelled.into()),
                    response = self.chat_completion_body(body, Some(&sink)) => response,
                }
            };
            let raw = raw.and_then(|response| {
                if let Some(usage) = &response.usage
                    && let Ok(usage) = decode_usage(usage)
                {
                    sink.publish_usage(usage);
                }
                decode_response(response).map_err(anyhow::Error::from)
            });
            let response = match raw {
                Ok(response) => response,
                Err(cause) => return Err(classify_error(cause)),
            };
            if !stream {
                emit_content(&response, events.as_ref());
            }
            sink.publish_usage(response.usage.clone());
            events.emit(ModelEvent::Finished {
                stop_reason: response.stop_reason.clone(),
            });
            Ok(response)
        }
        .await;
        if let Err(error) = &mut result {
            error.provider = self.config.provider_id.clone();
        }
        if let Err(error) = &result {
            events.emit(ModelEvent::Aborted {
                message: error.to_string(),
            });
        }
        result
    }
}

fn encode_image(source: ImageSource) -> Result<Value, ProviderError> {
    let url = match source {
        ImageSource::Url { url } if url.starts_with("https://") || url.starts_with("http://") => {
            url
        }
        ImageSource::Base64 { media_type, data } if !data.is_empty() => {
            let media_type =
                aicoder_core::media::image_media_type(&media_type).ok_or_else(|| {
                    error(
                        ProviderErrorKind::InvalidRequest,
                        "invalid image MIME declaration",
                    )
                })?;
            format!("data:{media_type};base64,{data}")
        }
        ImageSource::Artifact { .. } => {
            return Err(error(
                ProviderErrorKind::InvalidRequest,
                "tool image artifact must be resolved by the host before encoding",
            ));
        }
        _ => {
            return Err(error(
                ProviderErrorKind::InvalidRequest,
                "invalid inline or URL image",
            ));
        }
    };
    Ok(json!({"type":"image_url", "image_url":{"url":url}}))
}
fn flush_tool_images(messages: &mut Vec<Value>, images: &mut Vec<Value>) {
    if !images.is_empty() {
        let mut parts = vec![
            json!({"type":"text","text":"Tool image previews (data from preceding tool results)."}),
        ];
        parts.append(images);
        messages.push(json!({"role":"user","content":parts}));
    }
}

fn encode_request(request: ModelRequest) -> Result<Value, ProviderError> {
    let mut messages = Vec::new();
    let mut tool_images = Vec::new();
    for message in request.messages {
        if message.role == MessageRole::Tool {
            let anchor = message.tool_result().map(|(id, _, _)| id.to_owned());
            for block in message.content {
                match block {
                    ContentBlock::ToolResult {
                        call_id,
                        content,
                        is_error,
                    } => {
                        let content = if is_error {
                            json!({"is_error":true,"content":content}).to_string()
                        } else if let Value::String(text) = content {
                            text
                        } else {
                            content.to_string()
                        };
                        messages.push(
                            json!({"role":"tool", "tool_call_id":call_id, "content":content}),
                        );
                    }
                    ContentBlock::Image { source } => {
                        if anchor.is_none() {
                            return Err(error(
                                ProviderErrorKind::InvalidRequest,
                                "unanchored tool image",
                            ));
                        }
                        // Tool-role content is text-only in Chat Completions. Keep every
                        // parallel result before the ephemeral user image projection.
                        tool_images.push(json!({"type":"text","text":json!({"tool_call_id":anchor}).to_string()}));
                        tool_images.push(encode_image(source)?);
                    }
                    _ => {
                        return Err(error(
                            ProviderErrorKind::InvalidRequest,
                            "invalid tool content",
                        ));
                    }
                }
            }
            continue;
        }
        flush_tool_images(&mut messages, &mut tool_images);
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => unreachable!(),
        };
        let mut parts = Vec::new();
        let mut calls = Vec::new();
        let mut reasoning = String::new();
        let mut has_image = false;
        for block in message.content {
            match block {
                ContentBlock::Text { text } => parts.push(json!({"type":"text", "text":text})),
                ContentBlock::Reasoning { text } => reasoning.push_str(&text),
                ContentBlock::ToolCall { call } => calls
                    .push(json!({"id":call.id, "type":"function",
                    "function":{"name":call.name, "arguments":call.arguments.to_string()}})),
                ContentBlock::Image { source } => {
                    if role != "user" {
                        return Err(error(
                            ProviderErrorKind::Unsupported,
                            "this adapter only accepts images in user messages",
                        ));
                    }
                    has_image = true;
                    parts.push(encode_image(source)?);
                }
                ContentBlock::ProviderData { .. } => {
                    return Err(error(
                        ProviderErrorKind::Unsupported,
                        "chat-completions cannot replay opaque provider state",
                    ));
                }
                ContentBlock::ToolResult { .. } => {
                    return Err(error(
                        ProviderErrorKind::InvalidRequest,
                        "tool result outside a tool message",
                    ));
                }
            }
        }
        let content = if has_image {
            Value::Array(parts)
        } else {
            let text = parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<String>();
            if text.is_empty() && !calls.is_empty() {
                Value::Null
            } else {
                Value::String(text)
            }
        };
        let mut encoded = json!({"role":role, "content":content});
        if !calls.is_empty() {
            encoded["tool_calls"] = Value::Array(calls);
        }
        if !reasoning.is_empty() {
            encoded["reasoning_content"] = Value::String(reasoning);
        }
        messages.push(encoded);
    }
    flush_tool_images(&mut messages, &mut tool_images);
    let mut body = json!({"model":request.model, "messages":messages, "stream":request.stream});
    if request.stream {
        body["stream_options"] = json!({"include_usage":true});
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .into_iter()
                .map(|tool| {
                    let mut function = json!({"name":tool.name, "parameters":tool.parameters});
                    if let Some(description) = tool.description {
                        function["description"] = Value::String(description);
                    }
                    json!({"type":"function", "function":function})
                })
                .collect(),
        );
        body["tool_choice"] = match request.tool_choice {
            ModelToolChoice::Auto => json!("auto"),
            ModelToolChoice::None => json!("none"),
            ModelToolChoice::Required => json!("required"),
            ModelToolChoice::Named(name) => json!({"type":"function", "function":{"name":name}}),
        };
    }
    let opts = request.options;
    if let Some(effort) = opts.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    for (key, value) in [
        ("temperature", opts.temperature.map(|v| json!(v))),
        ("top_p", opts.top_p.map(|v| json!(v))),
        ("max_tokens", opts.max_output_tokens.map(|v| json!(v))),
        ("seed", opts.seed.map(|v| json!(v))),
        ("stop", opts.stop.map(|v| json!(v))),
    ] {
        if let Some(value) = value {
            body[key] = value;
        }
    }
    if let Some(format) = opts.response_format {
        body["response_format"] = match format {
            ResponseFormat::Text => json!({"type":"text"}),
            ResponseFormat::JsonObject => json!({"type":"json_object"}),
            ResponseFormat::JsonSchema {
                name,
                schema,
                strict,
            } => {
                json!({"type":"json_schema", "json_schema":{"name":name,"schema":schema,"strict":strict}})
            }
        };
    }
    Ok(body)
}

pub(crate) fn decode_call(call: &ToolCall) -> Result<ModelToolCall, ProviderError> {
    let arguments: Value = serde_json::from_str(&call.function.arguments).map_err(|_| {
        error(
            ProviderErrorKind::InvalidResponse,
            "tool arguments are not valid JSON",
        )
    })?;
    if call.id.is_empty() || call.function.name.is_empty() || !arguments.is_object() {
        return Err(error(
            ProviderErrorKind::InvalidResponse,
            "tool call needs an ID, name and JSON object arguments",
        ));
    }
    Ok(ModelToolCall {
        id: call.id.clone(),
        name: call.function.name.clone(),
        arguments,
    })
}

pub(crate) fn decode_usage(usage: &Usage) -> Result<ModelUsage, ProviderError> {
    let count = |v: i32| {
        u64::try_from(v)
            .map_err(|_| error(ProviderErrorKind::InvalidResponse, "negative token count"))
    };
    Ok(ModelUsage {
        input_tokens: Some(count(usage.prompt_tokens)?),
        output_tokens: Some(count(usage.completion_tokens)?),
        total_tokens: Some(count(usage.total_tokens)?),
        cached_input_tokens: usage
            .prompt_tokens_details
            .as_ref()
            .map(|v| count(v.cached_tokens))
            .transpose()?,
        cache_write_tokens: usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|v| v.cache_write_tokens)
            .map(count)
            .transpose()?,
    })
}

pub(crate) fn stop_reason(reason: &FinishReason) -> StopReason {
    match reason {
        FinishReason::Stop => StopReason::Complete,
        FinishReason::ToolCalls => StopReason::ToolCalls,
        FinishReason::Length => StopReason::MaxTokens,
        FinishReason::ContentFilter | FinishReason::Guard => StopReason::Filtered,
    }
}

fn decode_response(response: ChatCompletionResponse) -> Result<ModelResponse, ProviderError> {
    if response.choices.len() != 1 {
        return Err(error(
            ProviderErrorKind::InvalidResponse,
            "expected exactly one assistant response",
        ));
    }
    let choice = response.choices.into_iter().next().unwrap();
    if choice.index != 0 || choice.message.role != crate::wire::Role::Assistant {
        return Err(error(
            ProviderErrorKind::InvalidResponse,
            "expected assistant choice zero",
        ));
    }
    let stop_reason = choice
        .finish_reason
        .as_ref()
        .map(stop_reason)
        .ok_or_else(|| error(ProviderErrorKind::InvalidResponse, "missing finish reason"))?;
    let mut content = Vec::new();
    if let Some(text) = choice.message.reasoning.filter(|s| !s.is_empty()) {
        content.push(ContentBlock::Reasoning { text });
    }
    if let Some(text) = choice.message.content.filter(|s| !s.is_empty()) {
        content.push(ContentBlock::Text { text });
    }
    let mut ids = std::collections::HashSet::new();
    for call in choice.message.tool_calls.unwrap_or_default() {
        if !ids.insert(call.id.clone()) {
            return Err(error(
                ProviderErrorKind::InvalidResponse,
                "duplicate tool call ID",
            ));
        }
        content.push(ContentBlock::ToolCall {
            call: decode_call(&call)?,
        });
    }
    let has_calls = content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall { .. }));
    if (stop_reason == StopReason::ToolCalls) != has_calls {
        return Err(error(
            ProviderErrorKind::InvalidResponse,
            "finish reason and tool calls disagree",
        ));
    }
    if content.is_empty() && stop_reason == StopReason::Complete {
        return Err(error(
            ProviderErrorKind::InvalidResponse,
            "empty completed assistant response",
        ));
    }
    let response = ModelResponse {
        id: Some(response.id),
        model: response.model,
        message: Message {
            role: MessageRole::Assistant,
            content,
        },
        stop_reason,
        usage: response
            .usage
            .as_ref()
            .map(decode_usage)
            .transpose()?
            .unwrap_or_default(),
    };
    response.validate().map_err(|mut cause| {
        cause.provider = PROVIDER.into();
        cause
    })?;
    Ok(response)
}

fn emit_content(response: &ModelResponse, sink: &dyn ModelEventSink) {
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
}

struct NormalizedSink {
    events: Arc<dyn ModelEventSink>,
    usage: Mutex<Option<ModelUsage>>,
}
impl NormalizedSink {
    fn publish_usage(&self, usage: ModelUsage) {
        let mut previous = self.usage.lock().expect("usage lock poisoned");
        if previous.as_ref() != Some(&usage) {
            *previous = Some(usage.clone());
            self.events.emit(ModelEvent::Usage { usage });
        }
    }
}
impl ProviderEventSink for NormalizedSink {
    fn emit(&self, event: AgentRawEvent) {
        let normalized = match event {
            AgentRawEvent::ContentChunk {
                response_index: 0,
                delta,
            } => Some(ModelEvent::TextDelta {
                delta: delta.to_string(),
            }),
            AgentRawEvent::ReasoningChunk {
                response_index: 0,
                delta,
            } => Some(ModelEvent::ReasoningDelta {
                delta: delta.to_string(),
            }),
            AgentRawEvent::ToolCallChunk {
                key,
                id_delta,
                name_delta,
                arguments_delta,
            } if key.response_index == 0 => Some(ModelEvent::ToolCallDelta {
                index: key.tool_index as u32,
                id_delta: id_delta.map(|v| v.to_string()),
                name_delta: name_delta.map(|v| v.to_string()),
                arguments_delta: arguments_delta.map(|v| v.to_string()),
            }),
            AgentRawEvent::ToolCallEnded {
                key,
                tool_call: Some(call),
                ..
            } if key.response_index == 0 => Some(ModelEvent::ToolCallCompleted {
                index: key.tool_index as u32,
                call: (*call).clone(),
            }),
            AgentRawEvent::UsageUpdated { usage } => {
                self.publish_usage((*usage).clone());
                None
            }
            AgentRawEvent::ModelRetryScheduled {
                attempt,
                delay,
                reason,
            } => Some(ModelEvent::Retry {
                attempt,
                delay_ms: delay.as_millis().min(u128::from(u64::MAX)) as u64,
                reason: reason.to_string(),
            }),
            _ => None,
        };
        if let Some(event) = normalized {
            self.events.emit(event);
        }
    }
}

pub(crate) fn transport_error(message: impl AsRef<str>) -> ProviderError {
    let mut error = error(ProviderErrorKind::Transport, message);
    error.retryable = true;
    error
}

pub(crate) fn http_error(status: reqwest::StatusCode, message: impl AsRef<str>) -> ProviderError {
    let kind = match status.as_u16() {
        401 | 403 | 407 => ProviderErrorKind::Authentication,
        429 => ProviderErrorKind::RateLimited,
        408 | 500..=599 => ProviderErrorKind::Transport,
        _ => ProviderErrorKind::InvalidRequest,
    };
    let mut error = error(kind, message);
    error.http_status = Some(status.as_u16());
    error.retryable = matches!(status.as_u16(), 408 | 429 | 500..=599);
    error
}

fn classify_error(cause: anyhow::Error) -> ProviderError {
    if let Some(error) = cause.downcast_ref::<ProviderError>() {
        return error.clone();
    }
    let kind = if cause.downcast_ref::<TurnCancelled>().is_some() {
        ProviderErrorKind::Cancelled
    } else {
        ProviderErrorKind::InvalidResponse
    };
    error(kind, format!("{cause:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_images_follow_the_complete_parallel_result_batch_at_eof_or_before_next_message() {
        for next_message in [false, true] {
            let mut messages = vec![Message {
                role: MessageRole::Assistant,
                content: ["a", "b"]
                    .into_iter()
                    .map(|id| ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: id.into(),
                            name: "capture".into(),
                            arguments: json!({}),
                        },
                    })
                    .collect(),
            }];
            for id in ["a", "b"] {
                messages.push(Message {
                    role: MessageRole::Tool,
                    content: vec![
                        ContentBlock::ToolResult {
                            call_id: id.into(),
                            content: json!({"ok":true}),
                            is_error: id == "b",
                        },
                        ContentBlock::Image {
                            source: ImageSource::Base64 {
                                media_type: "image/jpeg".into(),
                                data: "AQID".into(),
                            },
                        },
                    ],
                });
            }
            if next_message {
                messages.push(Message::text(MessageRole::User, "continue"));
            }
            let request = ModelRequest::new("test", messages.clone());
            let mut spec = ModelSpec::new("fixture", "test");
            spec.capabilities.tools = true;
            spec.capabilities.images = true;
            request.validate(&spec).unwrap();
            let body = encode_request(request).unwrap();
            let wire = body["messages"].as_array().unwrap();
            assert_eq!(wire[1]["tool_call_id"], "a");
            assert_eq!(wire[2]["tool_call_id"], "b");
            assert!(wire[1]["content"].is_string());
            assert!(wire[2]["content"].is_string());
            assert_eq!(wire[3]["role"], "user");
            assert_eq!(
                wire[3]["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|p| p["type"] == "image_url")
                    .count(),
                2
            );
            assert_eq!(wire.len(), if next_message { 5 } else { 4 });
            assert_eq!(messages.len(), if next_message { 4 } else { 3 });
        }
    }
    #[tokio::test]
    async fn unresolved_artifact_images_fail_before_any_http_request() {
        let mut client = ChatClient::new(crate::ClientConfig {
            base_url: "http://127.0.0.1:1".into(),
            api_key: "fixture-only".into(),
            model: "test".into(),
            max_retries: 0,
            proxy: crate::ProxyMode::Disabled,
            ..Default::default()
        })
        .unwrap();
        let mut spec = ModelSpec::new("openai-compatible", "test");
        spec.capabilities.images = true;
        spec.capabilities.tools = true;
        client.register_model(spec).unwrap();
        let messages = vec![
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "a".into(),
                        name: "capture".into(),
                        arguments: json!({}),
                    },
                }],
            },
            Message {
                role: MessageRole::Tool,
                content: vec![
                    ContentBlock::ToolResult {
                        call_id: "a".into(),
                        content: json!({}),
                        is_error: false,
                    },
                    ContentBlock::Image {
                        source: ImageSource::Artifact {
                            reference: aicoder_core::artifacts::ArtifactRef {
                                version: 1,
                                id: "00000000-0000-0000-0000-000000000001".into(),
                                media_type: "image/png".into(),
                                bytes: 3,
                                complete: true,
                            },
                        },
                    },
                ],
            },
        ];
        let request = ModelRequest::new("test", messages);
        client.validate_request(&request).unwrap();
        let error = client
            .generate(request, Arc::new(()), TurnExecutionContext::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidRequest);
        assert!(error.message.contains("resolved by the host"));
    }

    #[test]
    fn wire_mime_normalization_does_not_rewrite_user_messages_or_forward_parameters() {
        let source = ImageSource::Base64 {
            media_type: " IMAGE/JpG ; url=https://must-not-fetch.invalid/a".into(),
            data: "AQID".into(),
        };
        let message = Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Image {
                source: source.clone(),
            }],
        };
        let original = message.clone();
        let body = encode_request(ModelRequest::new("test", vec![message])).unwrap();
        assert_eq!(
            body["messages"][0]["content"][0]["image_url"]["url"],
            "data:image/jpeg;base64,AQID"
        );
        assert_eq!(original.content, vec![ContentBlock::Image { source }]);
        assert!(
            encode_image(ImageSource::Base64 {
                media_type: "text/plain;image/png".into(),
                data: "AQID".into()
            })
            .is_err()
        );
        let preview = aicoder_core::tool_images::ResolvedToolImage::new(
            " IMAGE/JpG ; ignored=value".into(),
            "AQID".into(),
        )
        .unwrap();
        assert!(
            matches!(preview.source(),ImageSource::Base64{media_type,..} if media_type=="image/jpeg")
        );
    }

    fn response(message: Value, reason: &str) -> ChatCompletionResponse {
        serde_json::from_value(
            json!({"id":"r", "object":"chat.completion", "created":1, "model":"test",
            "choices":[{"index":0,"message":message,"finish_reason":reason}]}),
        )
        .unwrap()
    }

    #[test]
    fn malformed_tool_responses_and_empty_success_are_not_valid_completions() {
        for raw in [
            response(json!({"role":"assistant","content":""}), "stop"),
            response(json!({"role":"assistant","content":"   "}), "stop"),
            response(
                json!({"role":"assistant","reasoning":"analysis only"}),
                "stop",
            ),
            response(json!({"role":"assistant","content":"hello"}), "tool_calls"),
            response(
                json!({"role":"assistant","tool_calls":[{"id":"c","type":"function","function":{"name":"read","arguments":"broken"}}]}),
                "tool_calls",
            ),
            response(
                json!({"role":"assistant","tool_calls":[{"id":"c","type":"function","function":{"name":"read","arguments":"[]"}}]}),
                "tool_calls",
            ),
        ] {
            assert_eq!(
                decode_response(raw).unwrap_err().kind,
                ProviderErrorKind::InvalidResponse
            );
        }
    }

    #[test]
    fn truncated_and_filtered_outputs_preserve_the_stop_reason() {
        let truncated = decode_response(response(
            json!({"role":"assistant","content":"partial"}),
            "length",
        ))
        .unwrap();
        assert_eq!(truncated.stop_reason, StopReason::MaxTokens);
        assert_eq!(truncated.message.text_content(), "partial");
        assert_eq!(truncated.usage, ModelUsage::default());
        let filtered = decode_response(response(
            json!({"role":"assistant","content":null}),
            "content_filter",
        ))
        .unwrap();
        assert_eq!(filtered.stop_reason, StopReason::Filtered);
    }

    #[test]
    fn tool_result_errors_and_opaque_state_are_never_silently_dropped() {
        let request = ModelRequest::new(
            "test",
            vec![Message {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    call_id: "c".into(),
                    content: json!({"error":"missing file"}),
                    is_error: true,
                }],
            }],
        );
        let encoded = encode_request(request).unwrap();
        let content: Value =
            serde_json::from_str(encoded["messages"][0]["content"].as_str().unwrap()).unwrap();
        assert_eq!(content["is_error"], true);
        let opaque = ModelRequest::new(
            "test",
            vec![Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ProviderData {
                    provider: PROVIDER.into(),
                    data: json!({"signature":"private"}),
                }],
            }],
        );
        assert_eq!(
            encode_request(opaque).unwrap_err().kind,
            ProviderErrorKind::Unsupported
        );
    }
}
