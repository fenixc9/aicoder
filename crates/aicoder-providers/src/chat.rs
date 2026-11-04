//! OpenAI 兼容 API 客户端
use crate::wire::{ChatCompletionRequest, ChatCompletionResponse, StreamChunk, StreamOptions};

use anyhow::{Context, Result};
use futures::Stream;
use reqwest::{Client, StatusCode, header::HeaderMap};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aicoder_core::{
    cancellation::TurnExecutionContext,
    events::AgentRawEvent,
    redaction::{sanitize_text, sanitize_url},
};

use super::stream::{ChatStreamAccumulator, SseDecoder, SseEvent};
use crate::{
    BearerToken, ClientConfig, CredentialProvider, EnvironmentCredential, ProviderEventSink,
    StaticCredential,
};

const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
const BASE_RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    !error.is_builder() && !error.is_decode()
}

fn is_retryable_body_error(error: &reqwest::Error) -> bool {
    !error.is_builder()
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let delay = if let Ok(seconds) = value.trim().parse::<u64>() {
        Duration::from_secs(seconds)
    } else {
        httpdate::parse_http_date(value)
            .ok()?
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO)
    };
    Some(delay.min(MAX_RETRY_DELAY))
}

fn exponential_backoff(retry_number: u32) -> Duration {
    let exponent = retry_number.saturating_sub(1).min(16);
    let base_millis = BASE_RETRY_DELAY
        .as_millis()
        .saturating_mul(1_u128 << exponent)
        .min(MAX_RETRY_DELAY.as_millis()) as u64;
    let jitter_limit = base_millis / 4;
    let jitter = if jitter_limit == 0 {
        0
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64
            % (jitter_limit + 1)
    };
    Duration::from_millis((base_millis + jitter).min(MAX_RETRY_DELAY.as_millis() as u64))
}

fn retry_delay(headers: Option<&HeaderMap>, retry_number: u32) -> Duration {
    headers
        .and_then(retry_after)
        .unwrap_or_else(|| exponential_backoff(retry_number))
}

fn emit_retry(
    events: Option<&dyn ProviderEventSink>,
    attempt: u32,
    delay: Duration,
    reason: impl Into<String>,
) {
    if let Some(events) = events {
        events.emit(AgentRawEvent::ModelRetryScheduled {
            attempt,
            delay,
            reason: Arc::<str>::from(reason.into()),
        });
    }
}

/// OpenAI 兼容 API 客户端
pub struct ChatClient {
    pub(crate) transport_binding: Option<crate::transport::TransportBinding>,
    pub(crate) config: ClientConfig,
    http_client: Client,
    credentials: Arc<dyn CredentialProvider>,
    endpoint: reqwest::Url,
    pub(crate) proxy_secrets: Vec<String>,
    pub(crate) models: aicoder_core::model::ModelRegistry,
}

impl ChatClient {
    /// 从配置创建客户端
    pub fn new(mut config: ClientConfig) -> Result<Self> {
        let credentials: Arc<dyn CredentialProvider> = if !config.api_key.is_empty() {
            Arc::new(StaticCredential::new(std::mem::take(&mut config.api_key))?)
        } else if let Some(variable) = &config.api_key_env {
            Arc::new(EnvironmentCredential::new(variable.clone())?)
        } else {
            return Err(anyhow::anyhow!("API key is required"));
        };
        Self::with_credentials(config, credentials)
    }

    /// Inject a refreshable credential source; it is invoked for every HTTP attempt.
    pub fn with_credentials(
        mut config: ClientConfig,
        credentials: Arc<dyn CredentialProvider>,
    ) -> Result<Self> {
        config.validate()?;
        let endpoint = config.endpoint()?;
        config.api_key = String::new();
        let builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(config.timeout))
            .connect_timeout(Duration::from_secs(config.connect_timeout));
        let proxy_secrets = crate::transport_client::proxy_secrets(&config);
        let http_client = config
            .proxy
            .apply(builder)?
            .build()
            .map_err(|_| crate::config::invalid("Failed to create HTTP client"))?;
        Ok(Self {
            transport_binding: None,
            config,
            http_client,
            credentials,
            endpoint,
            proxy_secrets,
            models: aicoder_core::model::ModelRegistry::default(),
        })
    }
    /// Select another model on this exact configured service, reusing its HTTP pool and
    /// credential provider. Registered metadata is retained; other names use the same
    /// declarative protocol profile, never capabilities inferred from a model name.
    pub fn for_model(
        &self,
        model: &str,
    ) -> std::result::Result<Self, aicoder_core::model::ProviderError> {
        if model.trim().is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
            return Err(aicoder_core::model::ProviderError::new(
                &self.config.provider_id,
                aicoder_core::model::ProviderErrorKind::InvalidRequest,
                "model identity must be nonempty, single-line and at most 256 bytes",
            ));
        }
        let mut config = self.config.clone();
        config.model = model.to_string();
        Ok(Self {
            transport_binding: self.transport_binding.clone(),
            config,
            http_client: self.http_client.clone(),
            credentials: self.credentials.clone(),
            endpoint: self.endpoint.clone(),
            proxy_secrets: self.proxy_secrets.clone(),
            models: self.models.clone(),
        })
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }
    pub fn from_env(model: &str) -> Result<Self> {
        Self::new(ClientConfig::from_env(Some(model))?)
    }
    pub fn from_env_model(model: Option<&str>) -> Result<Self> {
        Self::new(ClientConfig::from_env(model)?)
    }
    fn transport_error(&self, message: impl AsRef<str>) -> aicoder_core::model::ProviderError {
        let mut error = crate::model::transport_error(message);
        error.provider = self.config.provider_id.clone();
        error
    }
    fn request_error(
        &self,
        token: &BearerToken,
        error: &reqwest::Error,
        phase: &str,
        attempt: u32,
    ) -> aicoder_core::model::ProviderError {
        let mut result = self.transport_error(format!(
            "{phase} failed after {attempt} attempt(s): {}{}",
            if error.is_timeout() {
                "request timed out: "
            } else {
                ""
            },
            self.diagnostic(token, &error.to_string(), false)
        ));
        result.retryable = is_retryable_transport_error(error);
        if error.is_timeout() {
            result.code = Some("timeout".into());
        }
        result
    }
    fn diagnostic(&self, token: &BearerToken, text: &str, truncated: bool) -> String {
        let mut text = self.config.proxy.redact(&token.redact(text, truncated));
        for secret in &self.proxy_secrets {
            text = crate::credentials::redact_value(&text, secret, truncated);
        }
        sanitize_text(&text, 8192)
    }

    /// 发起聊天完成请求（非流式）
    pub async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        self.chat_completion_inner(request, None).await
    }

    async fn chat_completion_inner(
        &self,
        request: ChatCompletionRequest,
        events: Option<&dyn ProviderEventSink>,
    ) -> Result<ChatCompletionResponse> {
        self.chat_completion_body(serde_json::to_value(request)?, events)
            .await
    }

    pub(crate) async fn chat_completion_body(
        &self,
        request: serde_json::Value,
        events: Option<&dyn ProviderEventSink>,
    ) -> Result<ChatCompletionResponse> {
        let prepared = self.prepare_transport(request, false, events).await?;
        let request = prepared.payload;
        let headers = prepared.headers;
        let session = prepared.session;
        let url = self.endpoint.to_string();
        let request_body = serde_json::to_string(&request)?;

        tracing::debug!(
            url = %sanitize_url(&url),
            model = %sanitize_text(request["model"].as_str().unwrap_or_default(), 256),
            messages = request["messages"].as_array().map_or(0, Vec::len),
            tools = request["tools"].as_array().map_or(0, Vec::len),
            stream = request["stream"].as_bool().unwrap_or(false),
            request_bytes = request_body.len(),
            "Sending chat completion request"
        );

        let total_attempts = self.config.max_retries.saturating_add(1);
        for attempt in 1..=total_attempts {
            tracing::debug!(attempt, total_attempts, "Starting HTTP attempt");
            self.transport_admit(session.as_ref())?;
            let token = self.credentials.bearer_token().await.map_err(|mut error| {
                error.provider = self.config.provider_id.clone();
                error
            })?;
            self.transport_ready(session.as_ref()).await?;
            let response = match self
                .http_client
                .post(&url)
                .headers(
                    headers
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.parse::<reqwest::header::HeaderName>()
                                    .expect("validated header"),
                                v.parse::<reqwest::header::HeaderValue>()
                                    .expect("validated header"),
                            )
                        })
                        .collect(),
                )
                .header("Authorization", token.header())
                .body(request_body.clone())
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) if attempt < total_attempts && is_retryable_transport_error(&error) => {
                    let delay = retry_delay(None, attempt);
                    let reason = self.diagnostic(&token, &error.to_string(), false);
                    emit_retry(events, attempt + 1, delay, reason.clone());
                    tracing::warn!(
                        attempt,
                        total_attempts,
                        retry_in_ms = delay.as_millis(),
                        error = %reason,
                        "HTTP transport error, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(error) => {
                    return Err(self
                        .request_error(&token, &error, "HTTP request", attempt)
                        .into());
                }
            };

            self.observe_transport_response(session.as_ref(), attempt, &response, &token)
                .await;
            let status = response.status();
            let delay_from_header = retry_after(response.headers());
            let request_id = response
                .headers()
                .get("x-request-id")
                .or_else(|| response.headers().get("openai-request-id"))
                .and_then(|value| value.to_str().ok())
                .map(|value| self.diagnostic(&token, value, false));
            let body = match read_body(
                response,
                if status.is_success() {
                    self.config.max_response_bytes
                } else {
                    MAX_ERROR_BODY_BYTES
                },
            )
            .await
            {
                Ok(body) => body,
                Err(error)
                    if attempt < total_attempts
                        && is_retryable_body_error(&error)
                        && (session.is_none() || !status.is_success()) =>
                {
                    let delay = delay_from_header.unwrap_or_else(|| retry_delay(None, attempt));
                    let reason = self.diagnostic(&token, &error.to_string(), false);
                    emit_retry(events, attempt + 1, delay, reason.clone());
                    tracing::warn!(
                        attempt,
                        total_attempts,
                        retry_in_ms = delay.as_millis(),
                        error = %reason,
                        "HTTP response body interrupted, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(error) => {
                    return Err(self
                        .request_error(&token, &error, "HTTP response body", attempt)
                        .into());
                }
            };

            let (body, truncated) = body;
            let body_text = String::from_utf8_lossy(&body);
            tracing::debug!(
                attempt,
                %status,
                request_id = request_id.as_deref(),
                response_bytes = body.len(),
                "Received chat completion response"
            );

            if status.is_success() {
                if truncated {
                    return Err(aicoder_core::model::ProviderError::new(
                        "openai-compatible",
                        aicoder_core::model::ProviderErrorKind::InvalidResponse,
                        "HTTP response exceeded max_response_bytes",
                    )
                    .into());
                }
                let resp: ChatCompletionResponse =
                    serde_json::from_slice(&body).map_err(|error| {
                        aicoder_core::model::ProviderError::new(
                            "openai-compatible",
                            aicoder_core::model::ProviderErrorKind::InvalidResponse,
                            format!(
                                "Failed to parse response body: {}",
                                self.diagnostic(&token, &error.to_string(), false)
                            ),
                        )
                    })?;
                tracing::debug!(
                    model = %self.diagnostic(&token,&resp.model,false),
                    prompt_tokens = resp.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                    completion_tokens = resp
                        .usage
                        .as_ref()
                        .map(|u| u.completion_tokens)
                        .unwrap_or(0),
                    attempts = attempt,
                    "Completed chat completion"
                );
                return Ok(resp);
            }

            if is_retryable_status(status) && attempt < total_attempts {
                let delay = delay_from_header.unwrap_or_else(|| retry_delay(None, attempt));
                emit_retry(events, attempt + 1, delay, format!("HTTP status {status}"));
                tracing::warn!(
                    attempt,
                    total_attempts,
                    %status,
                    retry_in_ms = delay.as_millis(),
                    "Retryable HTTP status, retrying"
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            return Err(crate::error::response_error(
                &self.config.provider_id,
                status,
                &body_text,
                attempt,
                truncated,
                |text| self.diagnostic(&token, text, truncated),
            )
            .into());
        }

        unreachable!("HTTP attempt loop always returns or errors")
    }

    /// 发起流式聊天完成请求
    pub async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<impl Stream<Item = Result<StreamChunk>>> {
        self.chat_completion_stream_inner(request, None).await
    }

    async fn chat_completion_stream_inner(
        &self,
        mut request: ChatCompletionRequest,
        events: Option<&dyn ProviderEventSink>,
    ) -> Result<impl Stream<Item = Result<StreamChunk>>> {
        request.stream = Some(true);
        request
            .stream_options
            .get_or_insert_with(StreamOptions::default);
        self.chat_completion_stream_body(serde_json::to_value(request)?, events)
            .await
    }

    async fn chat_completion_stream_body(
        &self,
        mut request: serde_json::Value,
        events: Option<&dyn ProviderEventSink>,
    ) -> Result<impl Stream<Item = Result<StreamChunk>>> {
        use futures::stream::StreamExt;
        if !self.config.stream_usage {
            request
                .as_object_mut()
                .context("invalid request body")?
                .remove("stream_options");
        }
        let prepared = self.prepare_transport(request, true, events).await?;
        let request = prepared.payload;
        let headers = prepared.headers;
        let session = prepared.session;
        let url = self.endpoint.to_string();
        let request_body = serde_json::to_string(&request)?;

        tracing::debug!(
            url = %sanitize_url(&url),
            model = %sanitize_text(request["model"].as_str().unwrap_or_default(), 256),
            messages = request["messages"].as_array().map_or(0, Vec::len),
            tools = request["tools"].as_array().map_or(0, Vec::len),
            request_bytes = request_body.len(),
            "Sending streaming chat completion request"
        );

        let total_attempts = self.config.max_retries.saturating_add(1);
        let (response, token, attempt) = self
            .send_streaming_request(
                &url,
                &request_body,
                total_attempts,
                events,
                &headers,
                session.as_ref(),
            )
            .await?;

        let mut response_stream = response.bytes_stream();
        let max_response_bytes = self.config.max_response_bytes;
        let parsed_stream = async_stream::try_stream! {
            let _session_owner=session.clone();
            let mut sequence=0u64;
            let mut response_bytes=0usize;
            let mut decoder = SseDecoder::default();
            let mut done = false;

            'streaming: while let Some(result) = response_stream.next().await {
                let bytes = result.map_err(|error| {
                    anyhow::Error::from(self.request_error(&token,&error,"HTTP stream",1))
                })?;
                let remaining=max_response_bytes.saturating_sub(response_bytes);
                let allowed=bytes.len().min(remaining);let exceeded=bytes.len()>remaining;
                response_bytes=response_bytes.saturating_add(allowed);
                for event in decoder.push(&bytes[..allowed])? {
                    match event {
                        SseEvent::Data(data) => {
                            sequence=sequence.saturating_add(1);
                            let chunk = serde_json::from_str::<StreamChunk>(&data)
                                .map_err(|error|aicoder_core::model::ProviderError::new("openai-compatible",aicoder_core::model::ProviderErrorKind::InvalidResponse,self.diagnostic(&token,&error.to_string(),false)));
                            if let Ok(chunk)=&chunk {self.transport_usage_before_observation(session.as_ref(),chunk.usage.as_ref(),events);}
                            self.observe_transport_event(session.as_ref(),attempt,sequence,&data,&token,events).await?;
                            yield chunk?;
                        }
                        SseEvent::Done => {
                            done = true;
                            break 'streaming;
                        }
                    }
                }
                if exceeded {Err(aicoder_core::model::ProviderError::new("openai-compatible",aicoder_core::model::ProviderErrorKind::InvalidResponse,"SSE response exceeded max_response_bytes"))?;}
            }

            if !done {
                for event in decoder.finish()? {
                    match event {
                        SseEvent::Data(data) => {
                            sequence=sequence.saturating_add(1);
                            let chunk = serde_json::from_str::<StreamChunk>(&data)
                                .map_err(|error|aicoder_core::model::ProviderError::new("openai-compatible",aicoder_core::model::ProviderErrorKind::InvalidResponse,self.diagnostic(&token,&error.to_string(),false)));
                            if let Ok(chunk)=&chunk {self.transport_usage_before_observation(session.as_ref(),chunk.usage.as_ref(),events);}
                            self.observe_transport_event(session.as_ref(),attempt,sequence,&data,&token,events).await?;
                            yield chunk?;
                        }
                        SseEvent::Done => break,
                    }
                }
            }
        };

        Ok(parsed_stream)
    }

    async fn send_streaming_request(
        &self,
        url: &str,
        request_body: &str,
        total_attempts: u32,
        events: Option<&dyn ProviderEventSink>,
        headers: &std::collections::BTreeMap<String, String>,
        session: Option<&Arc<crate::transport::Session>>,
    ) -> Result<(reqwest::Response, BearerToken, u32)> {
        for attempt in 1..=total_attempts {
            tracing::debug!(attempt, total_attempts, "Starting streaming HTTP attempt");
            self.transport_admit(session)?;
            let token = self.credentials.bearer_token().await.map_err(|mut error| {
                error.provider = self.config.provider_id.clone();
                error
            })?;
            self.transport_ready(session).await?;
            let response = match self
                .http_client
                .post(url)
                .header("Authorization", token.header())
                .headers(
                    headers
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.parse::<reqwest::header::HeaderName>()
                                    .expect("validated header"),
                                v.parse::<reqwest::header::HeaderValue>()
                                    .expect("validated header"),
                            )
                        })
                        .collect(),
                )
                .body(request_body.to_string())
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) if attempt < total_attempts && is_retryable_transport_error(&error) => {
                    let delay = retry_delay(None, attempt);
                    let reason = self.diagnostic(&token, &error.to_string(), false);
                    emit_retry(events, attempt + 1, delay, reason.clone());

                    tracing::warn!(
                        attempt,
                        total_attempts,
                        retry_in_ms = delay.as_millis(),
                        error = %reason,
                        "Streaming HTTP transport error, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(error) => {
                    return Err(self
                        .request_error(&token, &error, "Streaming HTTP request", attempt)
                        .into());
                }
            };

            self.observe_transport_response(session, attempt, &response, &token)
                .await;
            let status = response.status();
            if status.is_success() {
                return Ok((response, token, attempt));
            }

            let delay_from_header = retry_after(response.headers());
            let body = match read_body(
                response,
                if status.is_success() {
                    self.config.max_response_bytes
                } else {
                    MAX_ERROR_BODY_BYTES
                },
            )
            .await
            {
                Ok(body) => body,
                Err(error) if attempt < total_attempts && is_retryable_body_error(&error) => {
                    let delay = delay_from_header.unwrap_or_else(|| retry_delay(None, attempt));
                    let reason = self.diagnostic(&token, &error.to_string(), false);
                    emit_retry(events, attempt + 1, delay, reason.clone());
                    tracing::warn!(
                        attempt,
                        total_attempts,
                        retry_in_ms = delay.as_millis(),
                        error = %reason,
                        "Streaming error response interrupted, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(error) => {
                    return Err(self
                        .request_error(&token, &error, "Streaming HTTP error body", attempt)
                        .into());
                }
            };

            if is_retryable_status(status) && attempt < total_attempts {
                let delay = delay_from_header.unwrap_or_else(|| retry_delay(None, attempt));
                emit_retry(events, attempt + 1, delay, format!("HTTP status {status}"));
                tracing::warn!(
                    attempt,
                    total_attempts,
                    %status,
                    retry_in_ms = delay.as_millis(),
                    "Retryable streaming HTTP status, retrying"
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            let (body, truncated) = body;
            let body_text = String::from_utf8_lossy(&body);
            return Err(crate::error::response_error(
                &self.config.provider_id,
                status,
                &body_text,
                attempt,
                truncated,
                |text| self.diagnostic(&token, text, truncated),
            )
            .into());
        }

        unreachable!("streaming HTTP attempt loop always returns or errors")
    }

    /// Consume a streaming response and aggregate all deltas into one completion response.
    pub async fn chat_completion_stream_collect(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        use futures::StreamExt;

        request.stream = Some(true);
        let stream = self.chat_completion_stream(request).await?;
        futures::pin_mut!(stream);
        let mut accumulator = ChatStreamAccumulator::default();
        while let Some(chunk) = stream.next().await {
            accumulator.push(chunk?)?;
        }
        accumulator.finish()
    }

    #[cfg(test)]
    pub(crate) async fn chat_completion_stream_collect_with_events(
        &self,
        mut request: ChatCompletionRequest,
        events: &dyn ProviderEventSink,
    ) -> Result<ChatCompletionResponse> {
        use futures::StreamExt;

        request.stream = Some(true);
        let stream = self
            .chat_completion_stream_inner(request, Some(events))
            .await?;
        events.emit(AgentRawEvent::ModelResponseStarted);
        futures::pin_mut!(stream);
        let mut accumulator = ChatStreamAccumulator::default();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    if let Err(error) = accumulator.push_with_events(chunk, events) {
                        accumulator.abort_events(events, error.to_string());
                        return Err(error);
                    }
                }
                Err(error) => {
                    accumulator.abort_events(events, error.to_string());
                    return Err(error);
                }
            }
        }
        let response = accumulator.finish_with_events(events)?;
        events.emit(AgentRawEvent::ModelResponseCompleted {
            finish_reason: response
                .choices
                .first()
                .and_then(|choice| choice.finish_reason.as_ref().map(crate::model::stop_reason)),
        });
        Ok(response)
    }

    #[cfg(test)]
    pub(crate) async fn chat_completion_stream_collect_with_context(
        &self,
        mut request: ChatCompletionRequest,
        events: &dyn ProviderEventSink,
        context: &TurnExecutionContext,
    ) -> Result<ChatCompletionResponse> {
        request.stream = Some(true);
        request
            .stream_options
            .get_or_insert_with(StreamOptions::default);
        self.collect_stream_body(serde_json::to_value(request)?, events, context)
            .await
    }

    pub(crate) async fn collect_stream_body(
        &self,
        request: serde_json::Value,
        events: &dyn ProviderEventSink,
        context: &TurnExecutionContext,
    ) -> Result<ChatCompletionResponse> {
        use futures::StreamExt;
        let stream = tokio::select! {
            biased;
            cancelled = context.cancelled() => return Err(cancelled.into()),
            stream = self.chat_completion_stream_body(request, Some(events)) => stream?,
        };
        events.emit(AgentRawEvent::ModelResponseStarted);
        futures::pin_mut!(stream);
        let mut accumulator = ChatStreamAccumulator::default();
        loop {
            let chunk = tokio::select! {
                biased;
                cancelled = context.cancelled() => {
                    accumulator.abort_events(events, cancelled.reason());
                    return Err(cancelled.into());
                },
                chunk = stream.next() => chunk,
            };
            match chunk {
                Some(Ok(chunk)) => {
                    if let Err(error) = accumulator.push_with_events(chunk, events) {
                        accumulator.abort_events(events, error.to_string());
                        return Err(error);
                    }
                }
                Some(Err(error)) => {
                    accumulator.abort_events(events, error.to_string());
                    return Err(error);
                }
                None => break,
            }
        }
        let response = accumulator.finish_with_events(events)?;
        events.emit(AgentRawEvent::ModelResponseCompleted {
            finish_reason: response
                .choices
                .first()
                .and_then(|choice| choice.finish_reason.as_ref().map(crate::model::stop_reason)),
        });
        Ok(response)
    }
}

async fn read_body(
    response: reqwest::Response,
    limit: usize,
) -> std::result::Result<(Vec<u8>, bool), reqwest::Error> {
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = limit.saturating_sub(bytes.len());
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            return Ok((bytes, true));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProxyMode;
    use aicoder_core::events::{AgentEventEmitter, AgentRawEventEnvelope};
    use futures::StreamExt;
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for CapturedLogs {
        type Writer = CapturedWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            CapturedWriter(Arc::clone(&self.0))
        }
    }

    impl CapturedLogs {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    async fn spawn_mock_server(
        status: &str,
        content_type: &str,
        response_body: &str,
    ) -> (String, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let content_type = content_type.to_string();
        let response_body = response_body.to_string();
        let (request_sender, request_receiver) = oneshot::channel();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];

            let header_end = loop {
                let bytes_read = socket.read(&mut buffer).await.unwrap();
                assert!(bytes_read > 0, "client closed before sending HTTP headers");
                request.extend_from_slice(&buffer[..bytes_read]);

                if let Some(position) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    break position + 4;
                }
            };

            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);

            while request.len() < header_end + content_length {
                let bytes_read = socket.read(&mut buffer).await.unwrap();
                assert!(bytes_read > 0, "client closed before sending HTTP body");
                request.extend_from_slice(&buffer[..bytes_read]);
            }

            let captured_request = String::from_utf8(request).unwrap();
            let _ = request_sender.send(captured_request);

            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        });

        (format!("http://{address}/v1"), request_receiver)
    }

    struct MockHttpResponse {
        status: &'static str,
        content_type: &'static str,
        body: String,
        extra_headers: Vec<(&'static str, &'static str)>,
        declared_content_length: Option<usize>,
        close_without_response: bool,
    }

    impl MockHttpResponse {
        fn new(status: &'static str, content_type: &'static str, body: impl Into<String>) -> Self {
            Self {
                status,
                content_type,
                body: body.into(),
                extra_headers: Vec::new(),
                declared_content_length: None,
                close_without_response: false,
            }
        }

        fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
            self.extra_headers.push((name, value));
            self
        }

        fn with_declared_content_length(mut self, content_length: usize) -> Self {
            self.declared_content_length = Some(content_length);
            self
        }

        fn close_without_response() -> Self {
            Self {
                status: "",
                content_type: "",
                body: String::new(),
                extra_headers: Vec::new(),
                declared_content_length: None,
                close_without_response: true,
            }
        }
    }

    async fn spawn_mock_sequence(
        responses: Vec<MockHttpResponse>,
    ) -> (String, oneshot::Receiver<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_sender, request_receiver) = oneshot::channel();

        tokio::spawn(async move {
            let mut captured_requests = Vec::with_capacity(responses.len());
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];

                let header_end = loop {
                    let bytes_read = socket.read(&mut buffer).await.unwrap();
                    assert!(bytes_read > 0, "client closed before sending HTTP headers");
                    request.extend_from_slice(&buffer[..bytes_read]);
                    if let Some(position) = request.windows(4).position(|part| part == b"\r\n\r\n")
                    {
                        break position + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + content_length {
                    let bytes_read = socket.read(&mut buffer).await.unwrap();
                    assert!(bytes_read > 0, "client closed before sending HTTP body");
                    request.extend_from_slice(&buffer[..bytes_read]);
                }
                captured_requests.push(String::from_utf8(request).unwrap());

                if response.close_without_response {
                    socket.shutdown().await.unwrap();
                    continue;
                }

                let extra_headers = response
                    .extra_headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect::<String>();
                let content_length = response
                    .declared_content_length
                    .unwrap_or(response.body.len());
                let response_head = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {content_length}\r\n{extra_headers}Connection: close\r\n\r\n",
                    response.status, response.content_type
                );
                socket.write_all(response_head.as_bytes()).await.unwrap();
                socket.write_all(response.body.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
            let _ = request_sender.send(captured_requests);
        });

        (format!("http://{address}/v1"), request_receiver)
    }

    fn successful_response_body() -> &'static str {
        r#"{
            "id":"chatcmpl-retry",
            "object":"chat.completion",
            "created":123,
            "model":"deepseek-chat",
            "choices":[{
                "index":0,
                "message":{"role":"assistant","content":"recovered"},
                "finish_reason":"stop"
            }],
            "usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}
        }"#
    }

    fn test_client(base_url: String) -> ChatClient {
        test_client_with_retries(base_url, 0)
    }

    fn test_client_with_retries(base_url: String, max_retries: u32) -> ChatClient {
        ChatClient::new(ClientConfig {
            base_url,
            api_key: "test-key".to_string(),
            model: "deepseek-chat".to_string(),
            timeout: 2,
            max_retries,
            proxy: ProxyMode::Disabled,
            ..Default::default()
        })
        .unwrap()
    }

    fn test_request(stream: bool) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "deepseek-chat".to_string(),
            messages: vec![crate::wire::ChatMessage {
                role: crate::wire::Role::User,
                content: Some("hello".to_string()),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            temperature: Some(0.7),
            top_p: None,
            max_tokens: Some(32),
            seed: None,
            tools: None,
            tool_choice: None,
            stream: Some(stream),
            stream_options: None,
            stop: None,
            response_format: None,
        }
    }

    #[tokio::test]
    async fn normalized_model_api_encodes_images_schema_and_decodes_tool_calls() {
        use aicoder_core::model::*;
        let response = r#"{"id":"normalized","object":"chat.completion","created":1,"model":"deepseek-chat","choices":[{"index":0,"message":{"role":"assistant","reasoning":"thinking","tool_calls":[{"id":"call-1","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.rs\"}"}}]},"finish_reason":"tool_calls"}]}"#;
        let (url, captured) = spawn_mock_server("200 OK", "application/json", response).await;
        let mut client = test_client(url);
        let mut spec = ModelSpec::new("openai-compatible", "deepseek-chat");
        spec.capabilities = ModelCapabilities {
            images: true,
            tools: true,
            structured_output: true,
            ..Default::default()
        };
        client.register_model(spec).unwrap();
        let mut request = ModelRequest::new(
            "deepseek-chat",
            vec![Message {
                role: MessageRole::User,
                content: vec![
                    ContentBlock::Text {
                        text: "inspect".into(),
                    },
                    ContentBlock::Image {
                        source: ImageSource::Url {
                            url: "https://example.com/image.png".into(),
                        },
                    },
                ],
            }],
        );
        request.stream = false;
        request.tools.push(ToolDefinition {
            name: "read".into(),
            description: None,
            parameters: serde_json::json!({"type":"object"}),
        });
        request.tool_choice = ModelToolChoice::Named("read".into());
        request.options.response_format = Some(ResponseFormat::JsonSchema {
            name: "answer".into(),
            schema: serde_json::json!({"type":"object"}),
            strict: true,
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let result = client
            .generate(
                request,
                Arc::new(move |event| recorded.lock().unwrap().push(event)),
                TurnExecutionContext::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.stop_reason, StopReason::ToolCalls);
        assert_eq!(
            result.message.tool_calls().next().unwrap().arguments["path"],
            "a.rs"
        );
        assert_eq!(result.usage.total_tokens, None);
        let body: serde_json::Value =
            serde_json::from_str(captured.await.unwrap().split_once("\r\n\r\n").unwrap().1)
                .unwrap();
        assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
        assert_eq!(body["tool_choice"]["function"]["name"], "read");
        assert!(body["tools"][0]["function"].get("description").is_none());
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        let events = events.lock().unwrap();
        assert!(matches!(events.first(), Some(ModelEvent::Started)));
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Finished {
                stop_reason: StopReason::ToolCalls
            })
        ));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ModelEvent::Usage { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn normalized_stream_reports_deltas_and_one_terminal_usage() {
        use aicoder_core::model::*;
        let response = concat!(
            "data: {\"id\":\"s\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"}}]}\n\n",
            "data: {\"id\":\"s\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":1,\"total_tokens\":5}}\n\n",
            "data: [DONE]\n\n"
        );
        let (url, _) = spawn_mock_server("200 OK", "text/event-stream", response).await;
        let client = test_client(url);
        let request = ModelRequest::new(
            "deepseek-chat",
            vec![Message::text(MessageRole::User, "hello")],
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let result = client
            .generate(
                request,
                Arc::new(move |event| recorded.lock().unwrap().push(event)),
                TurnExecutionContext::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.message.text_content(), "hello");
        assert_eq!(result.usage.total_tokens, Some(5));
        let events = events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ModelEvent::TextDelta { delta } if delta == "hello"))
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ModelEvent::Usage { .. }))
                .count(),
            1
        );
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Finished {
                stop_reason: StopReason::Complete
            })
        ));
    }

    #[tokio::test]
    async fn normalized_api_rejects_unsupported_or_cancelled_requests_before_http() {
        use aicoder_core::model::*;
        let client = test_client("http://127.0.0.1:1".into());
        let mut request = ModelRequest::new(
            "deepseek-chat",
            vec![Message::text(MessageRole::User, "hello")],
        );
        request.messages[0].content.push(ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.com/a.png".into(),
            },
        });
        let error = client
            .generate(request.clone(), Arc::new(()), TurnExecutionContext::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Unsupported);
        let context = TurnExecutionContext::new();
        context.cancel("stop before request");
        let error = client
            .generate(request, Arc::new(()), context)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Cancelled);
    }

    #[tokio::test]
    async fn normalized_api_preserves_http_error_kind_status_and_redaction() {
        use aicoder_core::model::*;
        for (status, expected, retryable) in [
            ("401 Unauthorized", ProviderErrorKind::Authentication, false),
            (
                "429 Too Many Requests",
                ProviderErrorKind::RateLimited,
                true,
            ),
            (
                "503 Service Unavailable",
                ProviderErrorKind::Transport,
                true,
            ),
        ] {
            let (url, _) = spawn_mock_server(
                status,
                "application/json",
                r#"{"error":{"message":"api_key=private-secret"}}"#,
            )
            .await;
            let client = test_client_with_retries(url, 0);
            let mut request = ModelRequest::new(
                "deepseek-chat",
                vec![Message::text(MessageRole::User, "hello")],
            );
            request.stream = false;
            let error = client
                .generate(request, Arc::new(()), TurnExecutionContext::new())
                .await
                .unwrap_err();
            assert_eq!(error.kind, expected);
            assert_eq!(error.http_status, Some(status[..3].parse::<u16>().unwrap()));
            assert_eq!(error.retryable, retryable);
            assert!(!error.message.contains("private-secret"));
        }
    }

    #[tokio::test]
    async fn normalized_stream_cancellation_has_one_aborted_terminal_event() {
        use aicoder_core::model::*;
        let response = concat!(
            "data: {\"id\":\"s\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "data: {\"id\":\"s\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let (url, _) = spawn_mock_server("200 OK", "text/event-stream", response).await;
        let client = test_client(url);
        let request = ModelRequest::new(
            "deepseek-chat",
            vec![Message::text(MessageRole::User, "hello")],
        );
        let context = TurnExecutionContext::new();
        let cancellation = context.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let error = client
            .generate(
                request,
                Arc::new(move |event| {
                    if matches!(event, ModelEvent::TextDelta { .. }) {
                        cancellation.cancel("stop after partial text");
                    }
                    recorded.lock().unwrap().push(event);
                }),
                context,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Cancelled);
        let events = events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ModelEvent::Aborted { .. }))
                .count(),
            1
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ModelEvent::Finished { .. }))
        );
        assert!(matches!(events.last(), Some(ModelEvent::Aborted { .. })));
    }

    #[tokio::test]
    async fn normalized_incomplete_stream_retains_reported_usage_before_error() {
        use aicoder_core::model::*;
        let response = "data: {\"id\":\"s\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n";
        let (url, _) = spawn_mock_server("200 OK", "text/event-stream", response).await;
        let client = test_client(url);
        let request = ModelRequest::new(
            "deepseek-chat",
            vec![Message::text(MessageRole::User, "hello")],
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let error = client
            .generate(
                request,
                Arc::new(move |event| recorded.lock().unwrap().push(event)),
                TurnExecutionContext::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        let events = events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ModelEvent::Usage { usage } if usage.total_tokens == Some(7)))
        );
        assert!(matches!(events.last(), Some(ModelEvent::Aborted { .. })));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ModelEvent::Finished { .. }))
        );
    }

    #[tokio::test]
    async fn turn_executor_uses_the_normalized_http_provider_and_counts_usage_once() {
        use aicoder_core::{TurnExecutionConfig, TurnExecutor, model::*, tools::ToolRegistry};
        let (url, _) =
            spawn_mock_server("200 OK", "application/json", successful_response_body()).await;
        let executor = TurnExecutor::builder(test_client(url))
            .workspace(std::env::temp_dir())
            .registry(ToolRegistry::default())
            .config(TurnExecutionConfig {
                stream: false,
                ..Default::default()
            })
            .build()
            .unwrap();
        let result = executor
            .run(ModelRequest::new(
                "deepseek-chat",
                vec![Message::text(MessageRole::User, "hello")],
            ))
            .await
            .unwrap();
        assert_eq!(result.final_message.text_content(), "recovered");
        assert_eq!(result.usage.total_tokens, Some(3));
        assert_eq!(result.finish_reason, Some(StopReason::Complete));
    }

    #[tokio::test]
    async fn explicit_proxy_routes_the_request_without_resolving_the_origin() {
        let (proxy, captured) =
            spawn_mock_server("200 OK", "application/json", successful_response_body()).await;
        let config = ClientConfig {
            base_url: "http://origin.invalid/prefix/v1/".into(),
            api_key: "proxy-test-token".into(),
            model: "deepseek-chat".into(),
            proxy: ProxyMode::Explicit {
                url: proxy.trim_end_matches("/v1").into(),
                username: Some("proxy-user".into()),
                password: Some("proxy-password".into()),
            },
            max_retries: 0,
            ..Default::default()
        };
        let response = ChatClient::new(config)
            .unwrap()
            .chat_completion(test_request(false))
            .await
            .unwrap();
        assert_eq!(response.model, "deepseek-chat");
        let request = captured.await.unwrap();
        assert!(
            request.starts_with("POST http://origin.invalid/prefix/v1/chat/completions HTTP/1.1")
        );
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("proxy-authorization: basic "));
        assert!(lower.contains("authorization: bearer proxy-test-token"));
    }

    #[tokio::test]
    async fn credentials_are_refreshed_for_each_retry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (base_url, captured) = spawn_mock_sequence(vec![
            MockHttpResponse::new(
                "429 Too Many Requests",
                "application/json",
                r#"{"error":{"message":"retry"}}"#,
            )
            .with_header("Retry-After", "0"),
            MockHttpResponse::new("200 OK", "application/json", successful_response_body()),
        ])
        .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let source = Arc::new(move || {
            BearerToken::new(if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                "first-token"
            } else {
                "second-token"
            })
        });
        let client = ChatClient::with_credentials(
            ClientConfig {
                base_url,
                model: "deepseek-chat".into(),
                proxy: ProxyMode::Disabled,
                max_retries: 1,
                ..Default::default()
            },
            source,
        )
        .unwrap();
        client.chat_completion(test_request(false)).await.unwrap();
        let requests = captured.await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer first-token")
        );
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("authorization: bearer second-token")
        );
    }

    #[tokio::test]
    async fn nested_flat_string_and_plain_errors_keep_typed_details_and_hide_exact_secrets() {
        use aicoder_core::model::ProviderError;
        for stream in [false, true] {
            for (body, code, param) in [
                (
                    r#"{"error":{"message":"server echoed plain-private-token","type":"invalid_request_error","code":"bad_key","param":"model"}}"#,
                    Some("bad_key"),
                    Some("model"),
                ),
                (
                    r#"{"error_type":"invalid_request_error","message":"plain-private-token","code":400,"param":null}"#,
                    Some("400"),
                    None,
                ),
                (r#"{"error":"plain-private-token"}"#, None, None),
                ("plain-private-token", None, None),
            ] {
                let (base_url, _) =
                    spawn_mock_server("400 Bad Request", "application/json", body).await;
                let client = ChatClient::new(ClientConfig {
                    base_url,
                    api_key: "plain-private-token".into(),
                    proxy: ProxyMode::Disabled,
                    max_retries: 0,
                    ..Default::default()
                })
                .unwrap();
                let error = if stream {
                    client
                        .chat_completion_stream_collect(test_request(true))
                        .await
                        .unwrap_err()
                } else {
                    client
                        .chat_completion(test_request(false))
                        .await
                        .unwrap_err()
                };
                let error = error.downcast_ref::<ProviderError>().unwrap();
                assert_eq!(error.http_status, Some(400));
                assert_eq!(error.code.as_deref(), code);
                assert_eq!(error.parameter.as_deref(), param);
                assert!(!error.message.contains("plain-private-token"));
                assert!(error.message.contains("[REDACTED]"));
            }
        }
    }

    #[tokio::test]
    async fn response_limits_apply_to_success_and_error_bodies() {
        use aicoder_core::model::{ProviderError, ProviderErrorKind};
        let (base_url, _) =
            spawn_mock_server("200 OK", "application/json", successful_response_body()).await;
        let client = ChatClient::new(ClientConfig {
            base_url,
            api_key: "test-key".into(),
            proxy: ProxyMode::Disabled,
            max_retries: 0,
            max_response_bytes: 32,
            ..Default::default()
        })
        .unwrap();
        let error = client
            .chat_completion(test_request(false))
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ProviderError>().unwrap().kind,
            ProviderErrorKind::InvalidResponse
        );
        let body = "x".repeat(MAX_ERROR_BODY_BYTES + 1024);
        let (base_url, _) = spawn_mock_server("502 Bad Gateway", "text/plain", &body).await;
        let error = test_client(base_url)
            .chat_completion(test_request(false))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("truncated"));
        assert!(error.to_string().len() <= 4096);
    }

    #[tokio::test]
    async fn stream_options_can_be_disabled_and_byte_limits_preserve_complete_prefix_events() {
        use aicoder_core::model::*;
        let prefix = "data: {\"id\":\"s\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n";
        let body = format!(
            "{prefix}data: {}\n\n",
            serde_json::json!({"id":"s","object":"chat.completion.chunk","created":1,"model":"deepseek-chat","choices":[{"index":0,"delta":{"content":"x".repeat(1024)},"finish_reason":"stop"}]})
        );
        let (base_url, captured) = spawn_mock_server("200 OK", "text/event-stream", &body).await;
        let client = ChatClient::new(ClientConfig {
            base_url,
            api_key: "test-key".into(),
            model: "deepseek-chat".into(),
            proxy: ProxyMode::Disabled,
            max_retries: 0,
            stream_usage: false,
            max_response_bytes: prefix.len() + 4,
            ..Default::default()
        })
        .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let target = events.clone();
        let error = client
            .generate(
                ModelRequest::new(
                    "deepseek-chat",
                    vec![Message::text(MessageRole::User, "hello")],
                ),
                Arc::new(move |event| target.lock().unwrap().push(event)),
                TurnExecutionContext::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        let request = captured.await.unwrap();
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert!(body.get("stream_options").is_none());
        let events = events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event,ModelEvent::TextDelta {delta} if delta=="partial"))
        );
        assert!(matches!(events.last(), Some(ModelEvent::Aborted { .. })));
    }

    #[tokio::test]
    async fn deepseek_thinking_preserves_reasoning_and_declares_unsupported_options() {
        use aicoder_core::model::*;
        let (base_url, captured) =
            spawn_mock_server("200 OK", "application/json", successful_response_body()).await;
        let client = ChatClient::new(ClientConfig {
            base_url,
            api_key: "test-key".into(),
            model: "deepseek-chat".into(),
            proxy: ProxyMode::Disabled,
            max_retries: 0,
            compatibility: crate::CompatibilityProfile::DeepseekThinking,
            ..Default::default()
        })
        .unwrap();
        let mut request = ModelRequest::new(
            "deepseek-chat",
            vec![
                Message::text(MessageRole::User, "first"),
                Message {
                    role: MessageRole::Assistant,
                    content: vec![
                        ContentBlock::Reasoning {
                            text: "full earlier reasoning".into(),
                        },
                        ContentBlock::Text {
                            text: "first answer".into(),
                        },
                    ],
                },
                Message::text(MessageRole::User, "second"),
            ],
        );
        request.stream = false;
        client
            .generate(request.clone(), Arc::new(()), TurnExecutionContext::new())
            .await
            .unwrap();
        let captured = captured.await.unwrap();
        let body: serde_json::Value =
            serde_json::from_str(captured.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            body["messages"][1]["reasoning_content"],
            "full earlier reasoning"
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body.get("temperature").is_none());
        request.options.temperature = Some(0.7);
        let error = client
            .generate(request.clone(), Arc::new(()), TurnExecutionContext::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Unsupported);
        request.options.temperature = None;
        request.tools.push(ToolDefinition {
            name: "read".into(),
            description: None,
            parameters: serde_json::json!({"type":"object"}),
        });
        request.tool_choice = ModelToolChoice::Required;
        assert_eq!(
            client
                .generate(request, Arc::new(()), TurnExecutionContext::new())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Unsupported
        );
    }

    #[tokio::test]
    async fn credential_resolution_can_be_cancelled_before_an_http_attempt() {
        use aicoder_core::model::*;
        struct Pending(Arc<tokio::sync::Notify>);
        #[async_trait::async_trait]
        impl CredentialProvider for Pending {
            async fn bearer_token(&self) -> std::result::Result<BearerToken, ProviderError> {
                self.0.notify_one();
                std::future::pending().await
            }
        }
        for stream in [false, true] {
            let started = Arc::new(tokio::sync::Notify::new());
            let client = ChatClient::with_credentials(
                ClientConfig {
                    base_url: "http://127.0.0.1:1/v1".into(),
                    model: "test".into(),
                    proxy: ProxyMode::Disabled,
                    ..Default::default()
                },
                Arc::new(Pending(started.clone())),
            )
            .unwrap();
            let context = TurnExecutionContext::new();
            let cancellation = context.clone();
            let mut request =
                ModelRequest::new("test", vec![Message::text(MessageRole::User, "hello")]);
            request.stream = stream;
            let run =
                tokio::spawn(async move { client.generate(request, Arc::new(()), context).await });
            started.notified().await;
            cancellation.cancel("stop credential refresh");
            assert_eq!(
                run.await.unwrap().unwrap_err().kind,
                ProviderErrorKind::Cancelled
            );
        }
    }
    #[tokio::test]
    async fn named_profiles_keep_model_registration_and_errors_scoped() {
        use aicoder_core::model::*;
        let (base_url, _) = spawn_mock_server(
            "401 Unauthorized",
            "application/json",
            r#"{"error":{"message":"bad credentials","code":"bad_key"}}"#,
        )
        .await;
        let mut client = ChatClient::new(ClientConfig {
            base_url,
            provider_id: "my-gateway".into(),
            model: "test".into(),
            api_key: "test-key".into(),
            proxy: ProxyMode::Disabled,
            max_retries: 0,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(client.model_spec("test").unwrap().provider, "my-gateway");
        client
            .register_model(ModelSpec::new("my-gateway", "other"))
            .unwrap();
        assert!(
            client
                .register_model(ModelSpec::new("another-gateway", "wrong"))
                .is_err()
        );
        let mut request =
            ModelRequest::new("test", vec![Message::text(MessageRole::User, "hello")]);
        request.stream = false;
        let error = client
            .generate(request, Arc::new(()), TurnExecutionContext::new())
            .await
            .unwrap_err();
        assert_eq!(error.provider, "my-gateway");
        assert_eq!(error.code.as_deref(), Some("bad_key"));
    }

    #[tokio::test]
    async fn deepseek_thinking_tool_loop_replays_reasoning_through_core() {
        use aicoder_core::{TurnExecutionConfig, TurnExecutor, model::*, tools::*};
        struct Read;
        #[async_trait::async_trait]
        impl ExecutableTool for Read {
            fn definition(&self) -> ToolDefinition {
                ToolDefinition {
                    name: "read".into(),
                    description: None,
                    parameters: serde_json::json!({"type":"object"}),
                }
            }
            fn capability(&self) -> ToolCapability {
                ToolCapability::ReadOnly
            }
            async fn execute(
                &self,
                _: &ToolContext,
                _: serde_json::Value,
            ) -> std::result::Result<ToolSuccess, ToolFailure> {
                Ok(ToolSuccess {
                    artifacts: Vec::new(),
                    output: serde_json::json!("data"),
                    truncated: false,
                })
            }
        }
        let first=serde_json::json!({"id":"first","object":"chat.completion","created":1,"model":"deepseek-chat","choices":[{"index":0,"message":{"role":"assistant","reasoning_content":"required full reasoning","tool_calls":[{"id":"c","type":"function","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}).to_string();
        let (base_url, captured) = spawn_mock_sequence(vec![
            MockHttpResponse::new("200 OK", "application/json", &first),
            MockHttpResponse::new("200 OK", "application/json", successful_response_body()),
        ])
        .await;
        let client = ChatClient::new(ClientConfig {
            base_url,
            model: "deepseek-chat".into(),
            api_key: "test-key".into(),
            proxy: ProxyMode::Disabled,
            max_retries: 0,
            compatibility: crate::CompatibilityProfile::DeepseekThinking,
            ..Default::default()
        })
        .unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(Read).unwrap();
        let executor = TurnExecutor::builder(client)
            .workspace(std::env::temp_dir())
            .registry(registry)
            .config(TurnExecutionConfig {
                stream: false,
                ..Default::default()
            })
            .build()
            .unwrap();
        let result = executor
            .run(ModelRequest::new(
                "deepseek-chat",
                vec![Message::text(MessageRole::User, "read")],
            ))
            .await
            .unwrap();
        assert_eq!(result.rounds, 2);
        assert_eq!(result.final_message.text_content(), "recovered");
        let requests = captured.await.unwrap();
        let second: serde_json::Value =
            serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            second["messages"][1]["reasoning_content"],
            "required full reasoning"
        );
        assert_eq!(second["messages"][2]["tool_call_id"], "c");
    }

    #[tokio::test]
    async fn http_timeout_is_applied_and_reported_as_a_typed_transport_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let _socket = socket;
            std::future::pending::<()>().await;
        });
        let client = ChatClient::new(ClientConfig {
            base_url: format!("http://{address}/v1"),
            api_key: "test-key".into(),
            proxy: ProxyMode::Disabled,
            timeout: 1,
            connect_timeout: 1,
            max_retries: 0,
            ..Default::default()
        })
        .unwrap();
        let error = client
            .chat_completion(test_request(false))
            .await
            .unwrap_err();
        let error = error
            .downcast_ref::<aicoder_core::model::ProviderError>()
            .unwrap();
        assert_eq!(
            error.kind,
            aicoder_core::model::ProviderErrorKind::Transport
        );
        assert_eq!(error.code.as_deref(), Some("timeout"));
        assert!(error.message.contains("timed out"));
        server.abort();
    }

    #[test]
    fn test_client_config_default() {
        let config = ClientConfig::default();
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.timeout, 120);
        assert_eq!(config.max_retries, 3);
    }

    #[test]
    fn test_config_debug_and_serialization_redact_api_key() {
        let config = ClientConfig {
            base_url: "https://api.example.com/v1".to_string(),
            api_key: "test-key".to_string(),
            model: "gpt-3.5-turbo".to_string(),
            timeout: 60,
            max_retries: 1,
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("api.example.com"));
        assert!(json.contains("gpt-3.5-turbo"));
        assert!(!json.contains("test-key"));
        assert!(!json.contains("api_key"));

        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("test-key"));
    }

    #[test]
    fn test_client_rejects_empty_api_key() {
        let result = ChatClient::new(ClientConfig::default());
        assert!(result.is_err());
        assert_eq!(result.err().unwrap().to_string(), "API key is required");
    }

    #[tokio::test]
    async fn test_chat_completion_sends_request_and_parses_response() {
        let response_body = r#"{
            "id":"chatcmpl-test",
            "object":"chat.completion",
            "created":123,
            "model":"deepseek-chat",
            "choices":[{
                "index":0,
                "message":{"role":"assistant","content":"world"},
                "finish_reason":"stop"
            }],
            "usage":{
                "prompt_tokens":2,
                "completion_tokens":1,
                "total_tokens":3,
                "prompt_tokens_details":{"cached_tokens":1,"cache_write_tokens":0}
            }
        }"#;
        let (base_url, captured_request) =
            spawn_mock_server("200 OK", "application/json", response_body).await;

        let response = test_client(base_url)
            .chat_completion(test_request(false))
            .await
            .unwrap();

        assert_eq!(response.id, "chatcmpl-test");
        assert_eq!(response.model, "deepseek-chat");
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("world")
        );
        let usage = response.usage.unwrap();
        assert_eq!(usage.total_tokens, 3);
        assert_eq!(usage.cached_tokens(), 1);
        assert_eq!(usage.uncached_tokens(), 1);

        let captured_request = captured_request.await.unwrap();
        let (headers, body) = captured_request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key")
        );
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(body["messages"][0]["content"], "hello");
        assert_eq!(body["stream"], false);
    }

    #[tokio::test]
    async fn test_chat_completion_returns_structured_api_error() {
        let response_body =
            r#"{"error_type":"invalid_request_error","message":"bad model","code":400}"#;
        let (base_url, _) =
            spawn_mock_server("400 Bad Request", "application/json", response_body).await;

        let error = test_client(base_url)
            .chat_completion(test_request(false))
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "API error: invalid_request_error - bad model"
        );
    }

    #[tokio::test]
    async fn test_chat_completion_returns_server_error_body() {
        let (base_url, _) =
            spawn_mock_server("502 Bad Gateway", "text/plain", "upstream unavailable").await;

        let error = test_client(base_url)
            .chat_completion(test_request(false))
            .await
            .unwrap_err();
        let message = error.to_string();

        assert!(message.contains("502 Bad Gateway"));
        assert!(message.contains("upstream unavailable"));
    }

    #[tokio::test]
    async fn test_chat_completion_retries_server_error_and_rate_limit() {
        for retry_status in ["503 Service Unavailable", "429 Too Many Requests"] {
            let responses = vec![
                MockHttpResponse::new(retry_status, "text/plain", "retry later")
                    .with_header("Retry-After", "0"),
                MockHttpResponse::new("200 OK", "application/json", successful_response_body()),
            ];
            let (base_url, captured_requests) = spawn_mock_sequence(responses).await;

            let response = test_client_with_retries(base_url, 2)
                .chat_completion(test_request(false))
                .await
                .unwrap();

            assert_eq!(response.id, "chatcmpl-retry");
            assert_eq!(captured_requests.await.unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn test_chat_completion_stops_after_configured_retries() {
        let responses = (0..3)
            .map(|_| {
                MockHttpResponse::new("503 Service Unavailable", "text/plain", "still unavailable")
                    .with_header("Retry-After", "0")
            })
            .collect();
        let (base_url, captured_requests) = spawn_mock_sequence(responses).await;

        let error = test_client_with_retries(base_url, 2)
            .chat_completion(test_request(false))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("after 3 attempt(s)"));
        assert_eq!(captured_requests.await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_chat_completion_does_not_retry_non_retryable_status() {
        let responses = vec![MockHttpResponse::new(
            "400 Bad Request",
            "application/json",
            r#"{"error_type":"invalid_request_error","message":"bad request","code":400}"#,
        )];
        let (base_url, captured_requests) = spawn_mock_sequence(responses).await;

        let error = test_client_with_retries(base_url, 3)
            .chat_completion(test_request(false))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("bad request"));
        assert_eq!(captured_requests.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_chat_completion_retries_incomplete_response_headers() {
        let responses = vec![
            MockHttpResponse::close_without_response(),
            MockHttpResponse::new("200 OK", "application/json", successful_response_body()),
        ];
        let (base_url, captured_requests) = spawn_mock_sequence(responses).await;

        let response = test_client_with_retries(base_url, 1)
            .chat_completion(test_request(false))
            .await
            .unwrap();

        assert_eq!(response.id, "chatcmpl-retry");
        assert_eq!(captured_requests.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_chat_completion_retries_incomplete_response_body() {
        let responses = vec![
            MockHttpResponse::new("200 OK", "application/json", "{")
                .with_header("Retry-After", "0")
                .with_declared_content_length(100),
            MockHttpResponse::new("200 OK", "application/json", successful_response_body()),
        ];
        let (base_url, captured_requests) = spawn_mock_sequence(responses).await;

        let response = test_client_with_retries(base_url, 1)
            .chat_completion(test_request(false))
            .await
            .unwrap();

        assert_eq!(response.id, "chatcmpl-retry");
        assert_eq!(captured_requests.await.unwrap().len(), 2);
    }

    #[test]
    fn test_retry_after_supports_seconds_dates_and_cap() {
        assert!(is_retryable_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));

        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "5".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(5)));

        headers.insert(reqwest::header::RETRY_AFTER, "999".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(MAX_RETRY_DELAY));

        let future = SystemTime::now() + Duration::from_secs(5);
        headers.insert(
            reqwest::header::RETRY_AFTER,
            httpdate::fmt_http_date(future).parse().unwrap(),
        );
        let delay = retry_after(&headers).unwrap();
        assert!(delay >= Duration::from_secs(3));
        assert!(delay <= Duration::from_secs(5));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_chat_completion_does_not_log_or_return_credentials() {
        let secret = "sk-this-is-a-regression-test-secret";
        let response_body =
            format!("upstream echoed Authorization: Bearer {secret} and api_key={secret}");
        let (base_url, _) =
            spawn_mock_server("502 Bad Gateway", "text/plain", &response_body).await;
        let client = ChatClient::new(ClientConfig {
            base_url,
            api_key: secret.to_string(),
            model: "deepseek-chat".to_string(),
            timeout: 2,
            max_retries: 0,
            proxy: ProxyMode::Disabled,
            ..Default::default()
        })
        .unwrap();
        let mut request = test_request(false);
        request.messages[0].content = Some(format!("do not log {secret}"));

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .without_time()
            .with_writer(captured.clone())
            .finish();
        let error = client
            .chat_completion(request)
            .with_subscriber(subscriber)
            .await
            .unwrap_err();

        let logs = captured.contents();
        assert!(!logs.contains(secret), "secret leaked into logs: {logs}");
        assert!(logs.contains("request_bytes"));

        let error = error.to_string();
        assert!(!error.contains(secret), "secret leaked into error: {error}");
        assert!(error.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn test_chat_completion_reports_invalid_success_response() {
        let (base_url, _) = spawn_mock_server("200 OK", "application/json", "not-json").await;

        let error = test_client(base_url)
            .chat_completion(test_request(false))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .starts_with("Failed to parse response body")
        );
        assert_eq!(
            error
                .downcast_ref::<aicoder_core::model::ProviderError>()
                .unwrap()
                .kind,
            aicoder_core::model::ProviderErrorKind::InvalidResponse
        );
    }

    #[tokio::test]
    async fn test_chat_completion_stream_parses_sse_chunk() {
        let response_body = concat!(
            "data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",",
            "\"created\":123,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},",
            "\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",",
            "\"created\":123,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"content\":\" world\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let (base_url, captured_request) =
            spawn_mock_server("200 OK", "text/event-stream", response_body).await;

        let client = test_client(base_url);
        let stream = client
            .chat_completion_stream(test_request(true))
            .await
            .unwrap();
        futures::pin_mut!(stream);
        let chunk = stream.next().await.unwrap().unwrap();

        assert_eq!(chunk.id, "chatcmpl-stream");
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hello"));
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some(" world"));
        assert_eq!(
            chunk.choices[0].finish_reason,
            Some(crate::wire::FinishReason::Stop)
        );
        assert!(stream.next().await.is_none());

        let captured_request = captured_request.await.unwrap();
        let (headers, body) = captured_request.split_once("\r\n\r\n").unwrap();
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("accept: text/event-stream")
        );
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[tokio::test]
    async fn test_chat_completion_stream_collects_fragmented_tool_calls() {
        let response_body = concat!(
            "data: {\"id\":\"chatcmpl-tools\",\"object\":\"chat.completion.chunk\",",
            "\"created\":123,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"checking\",",
            "\"tool_calls\":[{\"index\":0,\"id\":\"call_\",\"type\":\"function\",",
            "\"function\":{\"name\":\"edit_\",\"arguments\":\"{\\\"pa\"}}]},",
            "\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-tools\",\"object\":\"chat.completion.chunk\",",
            "\"created\":123,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"1\",",
            "\"function\":{\"name\":\"file\",\"arguments\":\"th\\\":\\\"a.rs\\\"}\"}}]},",
            "\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"id\":\"chatcmpl-tools\",\"object\":\"chat.completion.chunk\",",
            "\"created\":123,\"model\":\"deepseek-chat\",\"choices\":[],",
            "\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}\n\n",
            "data: [DONE]\n\n"
        );
        let (base_url, _) = spawn_mock_server("200 OK", "text/event-stream", response_body).await;

        let response = test_client(base_url)
            .chat_completion_stream_collect(test_request(true))
            .await
            .unwrap();

        let message = &response.choices[0].message;
        assert_eq!(message.reasoning.as_deref(), Some("checking"));
        let call = &message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.id, "call_1");
        assert_eq!(call.function.name, "edit_file");
        assert_eq!(call.function.arguments, "{\"path\":\"a.rs\"}");
        assert_eq!(response.usage.unwrap().total_tokens, 12);
    }

    #[tokio::test]
    async fn cancellation_drops_inflight_stream_and_closes_open_content_event() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
            }
            let chunk = concat!(
                "data: {\"id\":\"cancel-stream\",\"object\":\"chat.completion.chunk\",",
                "\"created\":123,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,",
                "\"delta\":{\"role\":\"assistant\",\"content\":\"partial\"},",
                "\"finish_reason\":null}]}\n\n"
            );
            let headers = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/event-stream\r\n",
                "Transfer-Encoding: chunked\r\n",
                "Connection: close\r\n\r\n"
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket
                .write_all(format!("{:x}\r\n{chunk}\r\n", chunk.len()).as_bytes())
                .await
                .unwrap();
            socket.flush().await.unwrap();
            std::future::pending::<()>().await;
        });
        let client = test_client(format!("http://{address}"));
        let delivered = Arc::new(Mutex::new(Vec::<AgentRawEventEnvelope>::new()));
        let content_started = Arc::new(tokio::sync::Notify::new());
        let captured = Arc::clone(&delivered);
        let started = Arc::clone(&content_started);
        let events = AgentEventEmitter::new(Arc::new(move |event: &AgentRawEventEnvelope| {
            captured.lock().unwrap().push(event.clone());
            if matches!(event.event, AgentRawEvent::ContentChunk { .. }) {
                started.notify_one();
            }
        }));
        let context = TurnExecutionContext::new();
        let cancellation = context.clone();
        let cancel_task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(2), content_started.notified())
                .await
                .expect("stream content was not delivered");
            cancellation.cancel("stop streaming response");
        });

        let result = client
            .chat_completion_stream_collect_with_context(test_request(true), &events, &context)
            .await;
        cancel_task.await.unwrap();
        let error = result.unwrap_err();
        assert!(
            error
                .downcast_ref::<aicoder_core::TurnCancelled>()
                .is_some()
        );
        events.shutdown().await;
        server.abort();

        let delivered = delivered.lock().unwrap();
        assert!(delivered.iter().any(|event| matches!(
            &event.event,
            AgentRawEvent::ContentEnded {
                outcome: aicoder_core::events::StreamEnd::Aborted { reason },
                ..
            } if reason.as_ref() == "stop streaming response"
        )));
        assert!(
            !delivered
                .iter()
                .any(|event| matches!(event.event, AgentRawEvent::ModelResponseCompleted { .. }))
        );
    }

    #[tokio::test]
    async fn test_chat_completion_stream_retries_before_data_starts() {
        let stream_body = concat!(
            "data: {\"id\":\"chatcmpl-stream-retry\",\"object\":\"chat.completion.chunk\",",
            "\"created\":123,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"role\":\"assistant\",\"content\":\"recovered\"},",
            "\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let responses = vec![
            MockHttpResponse::new("503 Service Unavailable", "text/plain", "retry")
                .with_header("Retry-After", "0"),
            MockHttpResponse::new("200 OK", "text/event-stream", stream_body),
        ];
        let (base_url, captured_requests) = spawn_mock_sequence(responses).await;

        let delivered = Arc::new(Mutex::new(Vec::<AgentRawEventEnvelope>::new()));
        let captured = Arc::clone(&delivered);
        let events = AgentEventEmitter::new(Arc::new(move |event: &AgentRawEventEnvelope| {
            captured.lock().unwrap().push(event.clone());
        }));

        let response = test_client_with_retries(base_url, 1)
            .chat_completion_stream_collect_with_events(test_request(true), &events)
            .await
            .unwrap();
        events.shutdown().await;

        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("recovered")
        );
        assert_eq!(captured_requests.await.unwrap().len(), 2);
        let delivered = delivered.lock().unwrap();
        assert!(matches!(
            &delivered[0].event,
            AgentRawEvent::ModelRetryScheduled {
                attempt: 2,
                reason,
                ..
            } if reason.as_ref() == "HTTP status 503 Service Unavailable"
        ));
        assert!(matches!(
            delivered[1].event,
            AgentRawEvent::ModelResponseStarted
        ));
    }
    #[tokio::test]
    async fn reasoning_controls_encode_exact_declared_values_and_modern_output_caps() {
        use aicoder_core::{ModelProvider, TurnExecutionContext, model::*};
        let response = r#"{"id":"fixture","object":"chat.completion","created":1,"model":"deepseek-chat","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
        for effort in [
            None,
            Some(ReasoningEffort::None),
            Some(ReasoningEffort::Low),
            Some(ReasoningEffort::Max),
        ] {
            let (url, captured) = spawn_mock_server("200 OK", "application/json", response).await;
            let mut client = test_client(url);
            let mut spec = ModelSpec::new("openai-compatible", "deepseek-chat");
            spec.capabilities.reasoning_efforts = vec![
                ReasoningEffort::None,
                ReasoningEffort::Low,
                ReasoningEffort::Max,
            ];
            client.register_model(spec).unwrap();
            let mut request = ModelRequest::new(
                "deepseek-chat",
                vec![Message::text(MessageRole::User, "test")],
            );
            request.stream = false;
            request.options.reasoning_effort = effort;
            request.options.max_output_tokens = Some(128);
            client
                .generate(request, Arc::new(()), TurnExecutionContext::new())
                .await
                .unwrap();
            let raw = captured.await.unwrap();
            let body: serde_json::Value =
                serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(body["max_completion_tokens"], 128);
            assert!(body.get("max_tokens").is_none());
            if let Some(effort) = effort {
                assert_eq!(body["reasoning_effort"], effort.as_str());
            } else {
                assert!(body.get("reasoning_effort").is_none());
            }
        }
    }
    #[tokio::test]
    async fn unsupported_reasoning_never_resolves_a_credential_and_profiles_do_not_coerce() {
        use aicoder_core::{ModelProvider, TurnExecutionContext, model::*};
        struct NeverCredential;
        #[async_trait::async_trait]
        impl crate::CredentialProvider for NeverCredential {
            async fn bearer_token(&self) -> Result<crate::BearerToken, ProviderError> {
                panic!("invalid request must not acquire credentials")
            }
        }
        for compatibility in [
            crate::CompatibilityProfile::Generic,
            crate::CompatibilityProfile::DeepseekThinking,
            crate::CompatibilityProfile::DeepseekChat,
        ] {
            let client = ChatClient::with_credentials(
                ClientConfig {
                    model: "fixture".into(),
                    proxy: ProxyMode::Disabled,
                    compatibility,
                    ..Default::default()
                },
                Arc::new(NeverCredential),
            )
            .unwrap();
            let mut request =
                ModelRequest::new("fixture", vec![Message::text(MessageRole::User, "test")]);
            request.options.reasoning_effort = Some(ReasoningEffort::Low);
            let error = client
                .generate(request, Arc::new(()), TurnExecutionContext::new())
                .await
                .unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::Unsupported);
        }
        let mut client = ChatClient::new(ClientConfig {
            api_key: "fixture-key".into(),
            compatibility: crate::CompatibilityProfile::DeepseekThinking,
            proxy: ProxyMode::Disabled,
            ..Default::default()
        })
        .unwrap();
        let mut spec = ModelSpec::new("openai-compatible", "fixture");
        spec.capabilities.reasoning_efforts = vec![ReasoningEffort::Low];
        assert!(client.register_model(spec).is_err());
    }
    #[tokio::test]
    async fn explicit_deepseek_choices_keep_the_profile_wire_contract() {
        use aicoder_core::{ModelProvider, TurnExecutionContext, model::*};
        let response = r#"{"id":"fixture","object":"chat.completion","created":1,"model":"deepseek-chat","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
        for (compatibility, effort, mode) in [
            (
                crate::CompatibilityProfile::DeepseekThinking,
                ReasoningEffort::High,
                "enabled",
            ),
            (
                crate::CompatibilityProfile::DeepseekChat,
                ReasoningEffort::None,
                "disabled",
            ),
        ] {
            let (url, captured) = spawn_mock_server("200 OK", "application/json", response).await;
            let client = ChatClient::new(ClientConfig {
                base_url: url,
                api_key: "fixture-key".into(),
                model: "deepseek-chat".into(),
                proxy: ProxyMode::Disabled,
                compatibility,
                max_retries: 0,
                ..Default::default()
            })
            .unwrap();
            let mut request = ModelRequest::new(
                "deepseek-chat",
                vec![Message::text(MessageRole::User, "test")],
            );
            request.stream = false;
            request.options.reasoning_effort = Some(effort);
            request.options.max_output_tokens = Some(128);
            client
                .generate(request, Arc::new(()), TurnExecutionContext::new())
                .await
                .unwrap();
            let raw = captured.await.unwrap();
            let body: serde_json::Value =
                serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(body["thinking"]["type"], mode);
            assert_eq!(body["max_tokens"], 128);
            assert!(body.get("max_completion_tokens").is_none());
            if mode == "enabled" {
                assert_eq!(body["reasoning_effort"], "high");
            } else {
                assert!(body.get("reasoning_effort").is_none());
            }
        }
    }
}
