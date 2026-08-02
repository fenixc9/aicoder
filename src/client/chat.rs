//! OpenAI 兼容 API 客户端

use anyhow::{Context, Result};
use futures::Stream;
use reqwest::{Client, StatusCode, header::HeaderMap};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    events::{AgentEvent, AgentEventSink},
    redaction::{sanitize_text, sanitize_url},
    types::{ApiError, ChatCompletionRequest, ChatCompletionResponse, StreamChunk},
};

use super::stream::{ChatStreamAccumulator, SseDecoder, SseEvent};

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
    events: Option<&AgentEventSink>,
    attempt: u32,
    delay: Duration,
    reason: impl Into<String>,
) {
    if let Some(events) = events {
        events.emit(AgentEvent::ModelRetryScheduled {
            attempt,
            delay,
            reason: Arc::<str>::from(reason.into()),
        });
    }
}

/// API 客户端配置
#[derive(Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// API 基础 URL（如 https://api.openai.com/v1）
    pub base_url: String,
    /// API 密钥
    #[serde(skip_serializing)]
    pub api_key: String,
    /// 模型名称
    pub model: String,
    /// 超时时间（秒）
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// 重试次数
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientConfig")
            .field("base_url", &sanitize_url(&self.base_url))
            .field("api_key", &"[REDACTED]")
            .field("model", &sanitize_text(&self.model, 256))
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

fn default_timeout() -> u64 {
    120
}

fn default_max_retries() -> u32 {
    3
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: "gpt-4o".to_string(),
            timeout: 120,
            max_retries: 3,
        }
    }
}

/// OpenAI 兼容 API 客户端
pub struct ChatClient {
    config: ClientConfig,
    http_client: Client,
}

impl ChatClient {
    /// 从配置创建客户端
    pub fn new(config: ClientConfig) -> Result<Self> {
        if config.api_key.is_empty() {
            anyhow::bail!("API key is required");
        }

        let http_client = Client::builder()
            .no_proxy()
            .no_hickory_dns()
            .timeout(Duration::from_secs(config.timeout))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            config,
            http_client,
        })
    }

    /// 创建默认配置（从环境变量读取 API key）
    pub fn from_env(model: &str) -> Result<Self> {
        let base_url = std::env::var("OPENAI_API_BASE")
            .or_else(|_| std::env::var("DEEPSEEK_API_BASE"))
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

        let api_key = std::env::var("OPENAI_API_KEY")
            .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
            .or_else(|_| std::env::var("DASHSCOPE_API_KEY"))
            .or_else(|_| std::env::var("ZHIPU_API_KEY"))
            .context(
                "No API key found. Set OPENAI_API_KEY, DEEPSEEK_API_KEY, or another supported provider key.",
            )?;

        // 验证 base_url 格式
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            anyhow::bail!(
                "OPENAI_API_BASE must start with http:// or https://, got: {}",
                sanitize_url(&base_url)
            );
        }

        let config = ClientConfig {
            base_url,
            api_key,
            model: model.to_string(),
            ..Default::default()
        };

        Self::new(config)
    }

    /// 发起聊天完成请求（非流式）
    pub async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        self.chat_completion_inner(request, None).await
    }

    pub(crate) async fn chat_completion_with_events(
        &self,
        request: ChatCompletionRequest,
        events: &AgentEventSink,
    ) -> Result<ChatCompletionResponse> {
        self.chat_completion_inner(request, Some(events)).await
    }

    async fn chat_completion_inner(
        &self,
        request: ChatCompletionRequest,
        events: Option<&AgentEventSink>,
    ) -> Result<ChatCompletionResponse> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let request_body = serde_json::to_string(&request)?;

        tracing::debug!(
            url = %sanitize_url(&url),
            model = %sanitize_text(&request.model, 256),
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            stream = request.stream.unwrap_or(false),
            request_bytes = request_body.len(),
            "Sending chat completion request"
        );

        let total_attempts = self.config.max_retries.saturating_add(1);
        for attempt in 1..=total_attempts {
            tracing::debug!(attempt, total_attempts, "Starting HTTP attempt");
            let response = match self
                .http_client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", self.config.api_key))
                .body(request_body.clone())
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) if attempt < total_attempts && is_retryable_transport_error(&error) => {
                    let delay = retry_delay(None, attempt);
                    let reason = sanitize_text(&error.to_string(), 2048);
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
                    anyhow::bail!(
                        "HTTP request failed after {} attempt(s): {}",
                        attempt,
                        sanitize_text(&error.to_string(), 2048)
                    );
                }
            };

            let status = response.status();
            let delay_from_header = retry_after(response.headers());
            let request_id = response
                .headers()
                .get("x-request-id")
                .or_else(|| response.headers().get("openai-request-id"))
                .and_then(|value| value.to_str().ok())
                .map(|value| sanitize_text(value, 256));
            let body = match response.text().await {
                Ok(body) => body,
                Err(error) if attempt < total_attempts && is_retryable_body_error(&error) => {
                    let delay = delay_from_header.unwrap_or_else(|| retry_delay(None, attempt));
                    let reason = sanitize_text(&error.to_string(), 2048);
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
                    anyhow::bail!(
                        "Failed to read response body after {} attempt(s): {}",
                        attempt,
                        sanitize_text(&error.to_string(), 2048)
                    );
                }
            };

            tracing::debug!(
                attempt,
                %status,
                request_id = request_id.as_deref(),
                response_bytes = body.len(),
                "Received chat completion response"
            );

            if status.is_success() {
                let resp: ChatCompletionResponse =
                    serde_json::from_str(&body).context("Failed to parse response body")?;
                tracing::debug!(
                    model = %sanitize_text(&resp.model, 256),
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

            if status.is_server_error() {
                let body = sanitize_text(&body, MAX_ERROR_BODY_BYTES);
                return Err(anyhow::anyhow!(
                    "Server error (status {}) after {} attempt(s)\nBody: {}",
                    status,
                    attempt,
                    body
                ));
            }

            if let Ok(api_err) = serde_json::from_str::<ApiError>(&body) {
                let error_type = sanitize_text(&api_err.error_type, 256);
                let message = sanitize_text(&api_err.message, MAX_ERROR_BODY_BYTES);
                anyhow::bail!("API error: {} - {}", error_type, message);
            }

            let body = sanitize_text(&body, MAX_ERROR_BODY_BYTES);
            anyhow::bail!("API error (status {}): {}", status, body);
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
        events: Option<&AgentEventSink>,
    ) -> Result<impl Stream<Item = Result<StreamChunk>>> {
        use futures::stream::StreamExt;

        request.stream = Some(true);
        let url = format!("{}/chat/completions", self.config.base_url);
        let request_body = serde_json::to_string(&request)?;

        tracing::debug!(
            url = %sanitize_url(&url),
            model = %sanitize_text(&request.model, 256),
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            request_bytes = request_body.len(),
            "Sending streaming chat completion request"
        );

        let total_attempts = self.config.max_retries.saturating_add(1);
        let response = self
            .send_streaming_request(&url, &request_body, total_attempts, events)
            .await?;

        let mut response_stream = response.bytes_stream();
        let parsed_stream = async_stream::try_stream! {
            let mut decoder = SseDecoder::default();
            let mut done = false;

            'streaming: while let Some(result) = response_stream.next().await {
                let bytes = result.map_err(|error| {
                    anyhow::anyhow!(
                        "Stream transport error: {}",
                        sanitize_text(&error.to_string(), 2048)
                    )
                })?;
                for event in decoder.push(&bytes)? {
                    match event {
                        SseEvent::Data(data) => {
                            let chunk = serde_json::from_str::<StreamChunk>(&data)
                                .context("Failed to parse streaming response event")?;
                            yield chunk;
                        }
                        SseEvent::Done => {
                            done = true;
                            break 'streaming;
                        }
                    }
                }
            }

            if !done {
                for event in decoder.finish()? {
                    match event {
                        SseEvent::Data(data) => {
                            let chunk = serde_json::from_str::<StreamChunk>(&data)
                                .context("Failed to parse final streaming response event")?;
                            yield chunk;
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
        events: Option<&AgentEventSink>,
    ) -> Result<reqwest::Response> {
        for attempt in 1..=total_attempts {
            tracing::debug!(attempt, total_attempts, "Starting streaming HTTP attempt");
            let response = match self
                .http_client
                .post(url)
                .header("Authorization", format!("Bearer {}", self.config.api_key))
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .body(request_body.to_string())
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) if attempt < total_attempts && is_retryable_transport_error(&error) => {
                    let delay = retry_delay(None, attempt);
                    let reason = sanitize_text(&error.to_string(), 2048);
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
                    anyhow::bail!(
                        "Streaming HTTP request failed after {} attempt(s): {}",
                        attempt,
                        sanitize_text(&error.to_string(), 2048)
                    );
                }
            };

            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }

            let delay_from_header = retry_after(response.headers());
            let body = match response.text().await {
                Ok(body) => body,
                Err(error) if attempt < total_attempts && is_retryable_body_error(&error) => {
                    let delay = delay_from_header.unwrap_or_else(|| retry_delay(None, attempt));
                    let reason = sanitize_text(&error.to_string(), 2048);
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
                    anyhow::bail!(
                        "Failed to read streaming error response after {} attempt(s): {}",
                        attempt,
                        sanitize_text(&error.to_string(), 2048)
                    );
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

            let body = sanitize_text(&body, MAX_ERROR_BODY_BYTES);
            anyhow::bail!(
                "Streaming API error (status {}) after {} attempt(s): {}",
                status,
                attempt,
                body
            );
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

    pub(crate) async fn chat_completion_stream_collect_with_events(
        &self,
        mut request: ChatCompletionRequest,
        events: &AgentEventSink,
    ) -> Result<ChatCompletionResponse> {
        use futures::StreamExt;

        request.stream = Some(true);
        let stream = self
            .chat_completion_stream_inner(request, Some(events))
            .await?;
        events.emit(AgentEvent::ModelResponseStarted);
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
        events.emit(AgentEvent::ModelResponseCompleted {
            finish_reason: response
                .choices
                .first()
                .and_then(|choice| choice.finish_reason.clone()),
        });
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AgentEventEnvelope;
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
        })
        .unwrap()
    }

    fn test_request(stream: bool) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "deepseek-chat".to_string(),
            messages: vec![crate::types::ChatMessage {
                role: crate::types::Role::User,
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
            stop: None,
            response_format: None,
        }
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

        assert_eq!(error.to_string(), "Failed to parse response body");
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
            Some(crate::types::FinishReason::Stop)
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

        let delivered = Arc::new(Mutex::new(Vec::<AgentEventEnvelope>::new()));
        let captured = Arc::clone(&delivered);
        let events = AgentEventSink::new(Arc::new(move |event: &AgentEventEnvelope| {
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
            AgentEvent::ModelRetryScheduled {
                attempt: 2,
                reason,
                ..
            } if reason.as_ref() == "HTTP status 503 Service Unavailable"
        ));
        assert!(matches!(
            delivered[1].event,
            AgentEvent::ModelResponseStarted
        ));
    }
}
