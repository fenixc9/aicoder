//! OpenAI API 请求类型

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// 聊天消息角色
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 一个聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    /// 文本内容（assistant 回复或 user 输入时）
    pub content: Option<String>,
    /// 推理内容（某些模型如 qwen3 返回 reasoning 而非 content）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// 工具调用列表（仅 assistant 消息可能包含）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// 工具调用 ID（当 role 为 Tool 时）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 工具名称（当 role 为 Tool 时，由客户端填充）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 工具类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolType {
    Function,
}

/// 工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub tool_type: ToolType,
    pub function: FunctionDefinition,
}

/// 函数定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// 工具调用（模型返回）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// 调用 ID
    pub id: String,
    #[serde(rename = "type")]
    pub tool_type: ToolType,
    pub function: FunctionCall,
}

/// 函数调用参数（模型返回）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON 参数字符串
    pub arguments: String,
}

/// 采样策略 / 温度
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SamplingStrategy {
    /// 固定 temperature
    Temperature(f32),
    /// Top-p（核采样）
    TopP(f32),
    /// Top-k
    TopK(i32),
}

/// 流式响应标志
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamChoice {
    /// 普通非流式
    NonStream,
    /// 流式响应
    Stream(StreamFormat),
}

/// 流式输出格式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamFormat {
    /// 只流式输出内容，不流式输出 JSON
    Json,
}

/// Chat Completion 请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    /// 模型名称
    pub model: String,
    /// 消息列表
    pub messages: Vec<ChatMessage>,
    /// 采样参数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Top-p
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// 最大 token 数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    /// 种子（用于可重复性）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i32>,
    /// 工具列表
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    /// 工具调用模式
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// 是否流式
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// 停止序列
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// 响应格式
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseType>,
}

/// 工具调用模式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// 模型自主选择
    Auto,
    /// 强制使用某个工具
    Named(String),
    /// 强制使用所有工具
    Required,
}

/// 响应格式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseType {
    /// 纯文本
    Text,
    /// JSON 模式
    JsonObject,
}

/// 用户输入的封装
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserQuery {
    /// 用户消息内容
    pub content: String,
    /// 系统提示（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
}
