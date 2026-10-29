//! Token counting with explicit scope, precision and fallback provenance.
use crate::model::*;
use serde::{Deserialize, Serialize};
use std::{io::Write, sync::Arc};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    Request,
    Output,
    Text,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenAccuracy {
    Estimated,
    Exact,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenFallback {
    MissingTokenizer,
    TokenizerUnavailable,
    UnsupportedTokenizer,
    UnsupportedContent,
    InputLimit,
    CounterFailed,
    InvalidCount,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenCount {
    pub tokens: u64,
    pub scope: TokenScope,
    pub accuracy: TokenAccuracy,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<TokenFallback>,
}
impl TokenCount {
    pub fn estimated(tokens: u64, scope: TokenScope, method: impl Into<String>) -> Self {
        Self {
            tokens,
            scope,
            accuracy: TokenAccuracy::Estimated,
            method: method.into(),
            fallback: None,
        }
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.method.is_empty()
                && self.method.len() <= 128
                && self.method.is_ascii()
                && !self.method.chars().any(char::is_control),
            "invalid token count method"
        );
        anyhow::ensure!(
            self.fallback.is_none() || self.accuracy == TokenAccuracy::Estimated,
            "fallback count must be estimated"
        );
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub enum TokenContent<'a> {
    Request {
        messages: &'a [Message],
        tools: Option<&'a [ToolDefinition]>,
        response_format: Option<&'a ResponseFormat>,
        tool_choice: Option<&'a ModelToolChoice>,
    },
    Output(&'a Message),
    Text(&'a str),
}
impl TokenContent<'_> {
    pub fn scope(self) -> TokenScope {
        match self {
            Self::Request { .. } => TokenScope::Request,
            Self::Output(_) => TokenScope::Output,
            Self::Text(_) => TokenScope::Text,
        }
    }
}
#[derive(Clone, Copy)]
pub struct TokenInput<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub tokenizer: Option<&'a str>,
    pub content: TokenContent<'a>,
}
/// Pure counting only. Implementations must bound work and never send model requests.
pub trait TokenCounter: Send + Sync {
    fn count(&self, input: TokenInput<'_>) -> Result<TokenCount, TokenFallback>;
    /// Whether independent message costs can be subtracted during deterministic pruning.
    fn additive_input(&self) -> bool {
        false
    }
}
#[derive(Debug, Default)]
pub struct HeuristicTokenCounter;
impl TokenCounter for HeuristicTokenCounter {
    fn additive_input(&self) -> bool {
        true
    }
    fn count(&self, input: TokenInput<'_>) -> Result<TokenCount, TokenFallback> {
        let tokens = match input.content {
            TokenContent::Text(text) => text_tokens(text),
            TokenContent::Output(message) => message
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                        text_tokens(text)
                    }
                    _ => serialized_tokens(block),
                })
                .fold(0u64, u64::saturating_add),
            TokenContent::Request {
                messages,
                tools,
                response_format,
                tool_choice,
            } => {
                let mut tokens = messages
                    .iter()
                    .map(|message| serialized_tokens(message).saturating_add(4))
                    .fold(16u64, u64::saturating_add);
                if let Some(tools) = tools {
                    tokens = tokens.saturating_add(serialized_tokens(tools));
                }
                if let Some(format) = response_format {
                    tokens = tokens.saturating_add(serialized_tokens(format));
                }
                if let Some(choice) = tool_choice
                    && !matches!(choice, ModelToolChoice::Auto)
                {
                    tokens = tokens.saturating_add(serialized_tokens(choice));
                }
                tokens
            }
        };
        Ok(TokenCount::estimated(
            tokens,
            input.content.scope(),
            "heuristic_v1",
        ))
    }
}
// Counting UTF-8 leading bytes also works when serde splits multibyte text across writes.
struct Estimate {
    ascii: u64,
    unicode: u64,
}
impl Write for Estimate {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for byte in bytes {
            if byte.is_ascii() {
                self.ascii = self.ascii.saturating_add(1);
            } else if byte & 0xc0 != 0x80 {
                self.unicode = self.unicode.saturating_add(1);
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn serialized_tokens<T: Serialize + ?Sized>(value: &T) -> u64 {
    let mut estimate = Estimate {
        ascii: 0,
        unicode: 0,
    };
    serde_json::to_writer(&mut estimate, value).expect("domain token serialization");
    estimate
        .ascii
        .div_ceil(4)
        .saturating_add(estimate.unicode.saturating_mul(2))
}
pub fn text_tokens(text: &str) -> u64 {
    let mut estimate = Estimate {
        ascii: 0,
        unicode: 0,
    };
    estimate.write_all(text.as_bytes()).expect("count bytes");
    estimate
        .ascii
        .div_ceil(4)
        .saturating_add(estimate.unicode.saturating_mul(2))
}
/// Failures fall back explicitly; no invalid/missing count silently bypasses a budget.
#[derive(Clone)]
pub struct TokenCounting {
    counter: Arc<dyn TokenCounter>,
}
impl Default for TokenCounting {
    fn default() -> Self {
        Self::new(Arc::new(HeuristicTokenCounter))
    }
}
impl TokenCounting {
    pub fn new(counter: Arc<dyn TokenCounter>) -> Self {
        Self { counter }
    }
    pub fn additive_input(&self) -> bool {
        self.counter.additive_input()
    }
    pub fn count(&self, input: TokenInput<'_>) -> TokenCount {
        let result = self.counter.count(input).and_then(|count| {
            let nonempty = match input.content {
                TokenContent::Request {
                    messages, tools, ..
                } => !messages.is_empty() || tools.is_some_and(|tools| !tools.is_empty()),
                TokenContent::Text(text) => !text.is_empty(),
                TokenContent::Output(message) => message.content.iter().any(|block| match block {
                    ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                        !text.is_empty()
                    }
                    _ => true,
                }),
            };
            if count.validate().is_err()
                || count.scope != input.content.scope()
                || nonempty && count.tokens == 0
            {
                return Err(TokenFallback::InvalidCount);
            }
            Ok(count)
        });
        match result {
            Ok(count) => count,
            Err(reason) => {
                let mut count = HeuristicTokenCounter
                    .count(input)
                    .expect("heuristic counter");
                count.fallback = Some(reason);
                count
            }
        }
    }
    pub fn request(&self, request: &ModelRequest, spec: &ModelSpec) -> TokenCount {
        self.count(TokenInput {
            provider: &spec.provider,
            model: &request.model,
            tokenizer: spec.tokenizer.as_deref(),
            content: TokenContent::Request {
                messages: &request.messages,
                tools: Some(&request.tools),
                response_format: request.options.response_format.as_ref(),
                tool_choice: Some(&request.tool_choice),
            },
        })
    }
    pub fn request_view(
        &self,
        request: &ModelRequest,
        spec: &ModelSpec,
        messages: &[Message],
    ) -> TokenCount {
        self.count(TokenInput {
            provider: &spec.provider,
            model: &request.model,
            tokenizer: spec.tokenizer.as_deref(),
            content: TokenContent::Request {
                messages,
                tools: Some(&request.tools),
                response_format: request.options.response_format.as_ref(),
                tool_choice: Some(&request.tool_choice),
            },
        })
    }
    pub fn output(&self, spec: &ModelSpec, message: &Message) -> TokenCount {
        self.count(TokenInput {
            provider: &spec.provider,
            model: &spec.id,
            tokenizer: spec.tokenizer.as_deref(),
            content: TokenContent::Output(message),
        })
    }
    pub fn text(&self, spec: &ModelSpec, text: &str) -> TokenCount {
        self.count(TokenInput {
            provider: &spec.provider,
            model: &spec.id,
            tokenizer: spec.tokenizer.as_deref(),
            content: TokenContent::Text(text),
        })
    }
}
/// Exact text encoding. Request framing remains an estimate in TokenizedCounter.
pub trait TextTokenizer: Send + Sync {
    fn count_text(&self, text: &str, encoding: &str) -> Result<u64, TokenFallback>;
}
pub struct TokenizedCounter {
    tokenizer: Arc<dyn TextTokenizer>,
    encoding: Option<String>,
    max_bytes: usize,
}
impl TokenizedCounter {
    /// None selects only an explicit ModelSpec tokenizer hint, never a model-name guess.
    pub fn new(tokenizer: Arc<dyn TextTokenizer>, encoding: Option<String>) -> Self {
        Self {
            tokenizer,
            encoding,
            max_bytes: 2 * 1024 * 1024,
        }
    }
}
struct Buffer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.limit {
            return Err(std::io::Error::other("tokenizer input limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl TokenCounter for TokenizedCounter {
    fn count(&self, input: TokenInput<'_>) -> Result<TokenCount, TokenFallback> {
        let encoding = self
            .encoding
            .as_deref()
            .or(input.tokenizer)
            .ok_or(TokenFallback::MissingTokenizer)?;
        if encoding.len() > 64 || !encoding.is_ascii() || encoding.chars().any(char::is_control) {
            return Err(TokenFallback::UnsupportedTokenizer);
        }
        let mut remaining = self.max_bytes;
        let mut text = |text: &str| -> Result<u64, TokenFallback> {
            if text.len() > remaining {
                return Err(TokenFallback::InputLimit);
            }
            remaining -= text.len();
            self.tokenizer.count_text(text, encoding)
        };
        fn encoded<T: Serialize + ?Sized>(
            value: &T,
            limit: usize,
        ) -> Result<String, TokenFallback> {
            let mut buffer = Buffer {
                bytes: Vec::new(),
                limit,
            };
            serde_json::to_writer(&mut buffer, value).map_err(|_| TokenFallback::InputLimit)?;
            String::from_utf8(buffer.bytes).map_err(|_| TokenFallback::CounterFailed)
        }
        let mut exact = false;
        let tokens = match input.content {
            TokenContent::Text(value) => {
                exact = true;
                text(value)?
            }
            TokenContent::Output(message) => {
                let mut count = 0u64;
                for block in &message.content {
                    let cost = match block {
                        ContentBlock::Text { text: content }
                        | ContentBlock::Reasoning { text: content } => text(content)?,
                        ContentBlock::Image { .. } | ContentBlock::ProviderData { .. } => {
                            return Err(TokenFallback::UnsupportedContent);
                        }
                        block => text(&encoded(block, self.max_bytes)?)?,
                    };
                    count = count.saturating_add(cost);
                }
                count
            }
            TokenContent::Request {
                messages,
                tools,
                response_format,
                tool_choice,
            } => {
                let mut count = 16u64;
                for message in messages {
                    if message.content.iter().any(|block| {
                        matches!(
                            block,
                            ContentBlock::Image { .. } | ContentBlock::ProviderData { .. }
                        )
                    }) {
                        return Err(TokenFallback::UnsupportedContent);
                    }
                    count = count
                        .saturating_add(text(&encoded(message, self.max_bytes)?)?)
                        .saturating_add(4);
                }
                if let Some(tools) = tools {
                    count = count.saturating_add(text(&encoded(tools, self.max_bytes)?)?);
                }
                if let Some(format) = response_format {
                    count = count.saturating_add(text(&encoded(format, self.max_bytes)?)?);
                }
                if let Some(choice) = tool_choice
                    && !matches!(choice, ModelToolChoice::Auto)
                {
                    count = count.saturating_add(text(&encoded(choice, self.max_bytes)?)?);
                }
                count
            }
        };
        Ok(TokenCount {
            tokens,
            scope: input.content.scope(),
            accuracy: if exact {
                TokenAccuracy::Exact
            } else {
                TokenAccuracy::Estimated
            },
            method: format!("tokenizer:{encoding}"),
            fallback: None,
        })
    }
}

/// One shared reasoning adaptation for counting and provider projections.
pub(crate) fn model_messages<'a>(
    messages: &'a [Message],
    spec: &ModelSpec,
) -> std::borrow::Cow<'a, [Message]> {
    if spec.capabilities.reasoning_history
        || !messages.iter().any(|message| {
            message.role == MessageRole::Assistant
                && message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Reasoning { .. }))
        })
    {
        return std::borrow::Cow::Borrowed(messages);
    }
    std::borrow::Cow::Owned(
        messages
            .iter()
            .filter_map(|message| {
                let mut message = message.clone();
                if message.role == MessageRole::Assistant {
                    message
                        .content
                        .retain(|block| !matches!(block, ContentBlock::Reasoning { .. }));
                    if message.content.is_empty() {
                        return None;
                    }
                }
                Some(message)
            })
            .collect(),
    )
}
