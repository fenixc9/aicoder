//! OpenAI 兼容 API 客户端

use anyhow::{Context, Result};
use futures::Stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::types::{
    ApiError, ChatCompletionRequest, ChatCompletionResponse, StreamChunk,
};

/// API 客户端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// API 基础 URL（如 https://api.openai.com/v1）
    pub base_url: String,
    /// API 密钥
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
            .timeout(Duration::from_secs(config.timeout))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self { config, http_client })
    }

    /// 创建默认配置（从环境变量读取 API key）
    pub fn from_env(model: &str) -> Result<Self> {
        let base_url = std::env::var("OPENAI_API_BASE")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

        let api_key = std::env::var("OPENAI_API_KEY")
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
            .or_else(|_| std::env::var("DASHSCOPE_API_KEY"))
            .or_else(|_| std::env::var("ZHIPU_API_KEY"))
            .context("No API key found. Set OPENAI_API_KEY or other supported provider's key.")?;

        // 验证 base_url 格式
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            anyhow::bail!(
                "OPENAI_API_BASE must start with http:// or https://, got: {}",
                base_url
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
    pub async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let request_body = serde_json::to_string(&request)?;

        tracing::info!("Sending chat completion request to {}", url);
        tracing::info!("Request body: {}", request_body);

        let mut retry_count = 0;

        loop {
            let request_builder = self.http_client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", self.config.api_key))
                .body(request_body.clone());

            // 调试输出：显示完整请求
            tracing::debug!("Request URL: {}", url);
            tracing::debug!("Request headers: {}", format!("Bearer {}", self.config.api_key));

            let send_result = request_builder.send().await;

            match send_result {
                Ok(response) => {
                    let status = response.status();
                    let headers = response.headers().clone();
                    let body = response.text().await?;

                    eprintln!("\n--- HTTP Response ---");
                    eprintln!("Status: {}", status);
                    eprintln!("Headers: {:?}", headers);
                    eprintln!("Body: {}", body);
                    eprintln!("--- End ---\n");

                    if status.is_success() {
                        let resp: ChatCompletionResponse = serde_json::from_str(&body)
                            .context("Failed to parse response body")?;
                        tracing::info!(
                            "Completed with model={}, prompt_tokens={}, completion_tokens={}",
                            resp.model,
                            resp.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                            resp.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
                        );
                        return Ok(resp);
                    }

                    // 502/503 不重试，直接返回详细错误
                    if status.as_u16() >= 500 {
                        return Err(anyhow::anyhow!(
                            "Server error (status {})\nBody: {}",
                            status,
                            body
                        ));
                    }

                    // 尝试解析错误
                    if let Ok(api_err) = serde_json::from_str::<ApiError>(&body) {
                        anyhow::bail!("API error: {} - {}", api_err.error_type, api_err.message);
                    }

                    anyhow::bail!("API error (status {}): {}", status, body);
                }
                Err(e) => {
                    retry_count += 1;
                    if retry_count <= self.config.max_retries {
                        tracing::warn!("Network error, retrying: {}", e);
                        tokio::time::sleep(Duration::from_secs(2_u64.pow(retry_count))).await;
                        continue;
                    }
                    anyhow::bail!("Failed after {} retries: {}", retry_count, e);
                }
            }
        }
    }

    /// 发起流式聊天完成请求
    pub async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<impl Stream<Item = Result<StreamChunk>>> {
        use futures::stream::StreamExt;

        let url = format!("{}/chat/completions", self.config.base_url);
        let request_body = serde_json::to_string(&request)?;

        tracing::info!("Sending streaming chat completion request to {}", url);

        let response = self.http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(request_body)
            .send()
            .await
            .context("Failed to send streaming request")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            anyhow::bail!("API error (status {}): {}", status, body);
        }

        let resp_stream = response.bytes_stream();

        // 将流中的每个 chunk 解析为 StreamChunk
        let parsed_stream = resp_stream.map(|result| {
            match result {
                Ok(bytes) => {
                    let text = match String::from_utf8(bytes.to_vec()) {
                        Ok(t) => t,
                        Err(_) => return Ok(None),
                    };

                    for line in text.lines() {
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data.trim() == "[DONE]" {
                                return Ok(None);
                            }
                            if let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) {
                                return Ok(Some(chunk));
                            }
                        }
                    }
                    Ok(None)
                }
                Err(e) => Err(e),
            }
        })
        .filter_map(|result| async move {
            match result {
                Ok(Some(chunk)) => Some(Ok(chunk)),
                Ok(None) => None,
                Err(e) => Some(Err(anyhow::anyhow!("Stream error: {}", e))),
            }
        });

        Ok(parsed_stream)
    }

    /// 判断是否应该重试请求
    fn should_retry(&self, status: reqwest::StatusCode, last_error: &Option<ApiError>) -> bool {
        // 429 限流和 5xx 错误需要重试
        status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || last_error.as_ref().is_some_and(|e| e.error_type == "rate_limit")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_config_default() {
        let config = ClientConfig::default();
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.timeout, 120);
        assert_eq!(config.max_retries, 3);
    }

    #[test]
    fn test_config_serialization() {
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
    }
}
