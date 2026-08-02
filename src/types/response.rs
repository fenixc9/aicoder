//! OpenAI API 响应类型

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use super::request::{ChatMessage, Role, ToolType};

/// Chat Completion 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    /// 响应 ID
    pub id: String,
    /// 对象类型（固定为 "chat.completion"）
    #[serde(rename = "object")]
    pub object_type: String,
    /// 创建时间戳
    pub created: i64,
    /// 模型名称
    pub model: String,
    /// 模型服务信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_service: Option<String>,
    /// Choices 列表
    pub choices: Vec<Choice>,
    /// 日志概率信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<LogProbs>,
    /// 使用量统计
    pub usage: Option<Usage>,
}

/// 一个 choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    /// 索引
    pub index: i32,
    /// 消息内容
    pub message: ChatMessage,
    /// 停止原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// 日志概率信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<LogProbs>,
}

/// 停止原因
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FinishReason {
    Stop,
    Length,
    /// 模型返回了工具调用
    #[serde(rename = "tool_calls")]
    ToolCalls,
    ContentFilter,
    /// 模型拒绝生成（安全原因）
    /// 注：OpenAI 新版 API 中已不再使用 `guard` 作为 finish_reason
    /// 此处保留以防需要兼容某些代理实现
    /// #[serde(rename = "guard")]
    Guard,
}

/// 日志概率
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogProbs {
    pub token_logprobs: Vec<f64>,
    pub tokens: Vec<String>,
    pub top_logprobs: Vec<serde_json::Value>,
    pub token_offsets: Vec<TokenOffset>,
}

/// Token 偏移
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenOffset {
    pub start: i32,
    pub end: i32,
}

/// 使用量统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// 提示词 token 数
    pub prompt_tokens: i32,
    /// 生成 token 数
    pub completion_tokens: i32,
    /// 总 token 数
    pub total_tokens: i32,
    /// Prompt token 明细，包括缓存读取和缓存写入 token
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptTokensDetails {
    /// 从 prompt cache 读取的 token 数
    #[serde(default)]
    pub cached_tokens: i32,
    /// 写入 prompt cache 的 token 数；只有部分模型提供
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i32>,
}

impl Usage {
    pub fn cached_tokens(&self) -> i32 {
        self.prompt_tokens_details
            .as_ref()
            .map(|details| details.cached_tokens)
            .unwrap_or(0)
    }

    pub fn uncached_tokens(&self) -> i32 {
        (self.prompt_tokens - self.cached_tokens()).max(0)
    }

    pub fn cache_hit_rate(&self) -> f64 {
        if self.prompt_tokens == 0 {
            0.0
        } else {
            f64::from(self.cached_tokens()) / f64::from(self.prompt_tokens)
        }
    }

    pub fn accumulate(&mut self, other: &Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;

        if let Some(other_details) = &other.prompt_tokens_details {
            let details = self
                .prompt_tokens_details
                .get_or_insert_with(PromptTokensDetails::default);
            details.cached_tokens += other_details.cached_tokens;
            if let Some(cache_write_tokens) = other_details.cache_write_tokens {
                *details.cache_write_tokens.get_or_insert(0) += cache_write_tokens;
            }
        }
    }
}

/// SSE 流式事件
#[derive(Debug, Clone, Deserialize)]
pub struct StreamChunk {
    /// 事件 ID
    pub id: String,
    /// 对象类型
    #[serde(rename = "object")]
    pub object_type: String,
    /// 创建时间
    pub created: i64,
    /// 模型名称
    pub model: String,
    /// 服务名称
    pub service_tier: Option<String>,
    /// 系统厂商
    pub system: Option<String>,
    /// Choices 列表
    pub choices: Vec<StreamChoice>,
    /// 日志概率
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<LogProbs>,
    /// 使用量统计（仅在最后一个事件中出现）
    pub usage: Option<Usage>,
}

/// 流式 Choice
#[derive(Debug, Clone, Deserialize)]
pub struct StreamChoice {
    /// 索引
    pub index: i32,
    /// Delta 内容
    pub delta: StreamDelta,
    /// 停止原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// 日志概率
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<LogProbs>,
}

/// 流式 Delta
#[derive(Debug, Clone, Deserialize)]
pub struct StreamDelta {
    /// 角色
    pub role: Option<Role>,
    /// 内容
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 推理内容；部分兼容服务使用 reasoning_content
    #[serde(default, alias = "reasoning_content")]
    pub reasoning: Option<String>,
    /// 工具调用
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<StreamToolCall>>,
    /// 工具调用 ID（当 delta 包含 tool_calls 时）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// 流式工具调用
#[derive(Debug, Clone, Deserialize)]
pub struct StreamToolCall {
    /// 工具调用索引
    pub index: i32,
    /// 工具调用 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub tool_type: Option<ToolType>,
    pub function: Option<StreamFunctionCall>,
}

/// 流式函数调用
#[derive(Debug, Clone, Deserialize)]
pub struct StreamFunctionCall {
    /// 函数名
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 参数 JSON 字符串
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// API 错误
#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    /// 错误类型
    pub error_type: String,
    /// 错误消息
    pub message: String,
    /// 错误码（如 429, 500）
    pub code: Option<i32>,
    /// 错误参数详情
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
}

/// Embedding 请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    /// 模型名称
    pub model: String,
    /// 输入文本或字符串数组
    pub input: EmbeddingInput,
    /// 输出维度
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<i32>,
    /// 格式
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<EncodingFormat>,
}

/// Embedding 输入
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    /// 单个字符串
    Single(String),
    /// 字符串数组
    Multiple(Vec<String>),
}

/// 编码格式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncodingFormat {
    Float,
    Base64,
}

/// Embedding 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    /// 对象类型
    #[serde(rename = "object")]
    pub object_type: String,
    /// 数据列表
    pub data: Vec<EmbeddingData>,
    /// 模型名称
    pub model: String,
    /// 使用量
    pub usage: Option<Usage>,
}

/// Embedding 数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingData {
    /// 对象类型
    #[serde(rename = "object")]
    pub object_type: String,
    /// 索引
    pub index: i32,
    /// 嵌入向量
    pub embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_accepts_missing_or_null_prompt_details() {
        let missing: Usage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "total_tokens": 12
        }))
        .unwrap();
        assert_eq!(missing.cached_tokens(), 0);
        assert_eq!(missing.uncached_tokens(), 10);

        let null: Usage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "total_tokens": 12,
            "prompt_tokens_details": null
        }))
        .unwrap();
        assert_eq!(null.cached_tokens(), 0);
    }

    #[test]
    fn usage_calculates_cache_hit_rate() {
        let usage: Usage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 2000,
            "completion_tokens": 100,
            "total_tokens": 2100,
            "prompt_tokens_details": {
                "cached_tokens": 1500,
                "cache_write_tokens": 200
            }
        }))
        .unwrap();

        assert_eq!(usage.cached_tokens(), 1500);
        assert_eq!(usage.uncached_tokens(), 500);
        assert!((usage.cache_hit_rate() - 0.75).abs() < f64::EPSILON);
    }
}
