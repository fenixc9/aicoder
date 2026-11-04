//! Ephemeral provider-neutral request views; canonical messages and routing stay immutable.
use super::*;
use crate::{model::*, tokens::TokenCount};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const REQUEST_VIEW_NAMESPACE: &str = "aicoder.request-view.v1";
pub const MAX_REQUEST_VIEW_BYTES: usize = 16 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestHookScope {
    Conversation,
    SystemAndConversation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestHookPass {
    Preparation,
    Admission,
    AfterCompaction,
}
pub struct RequestHookInput<'a> {
    pub workspace: &'a Path,
    pub round: usize,
    pub pass: RequestHookPass,
    pub request: &'a ModelRequest,
    pub history: &'a [Message],
    pub system_prefix_len: usize,
    pub spec: &'a ModelSpec,
}
#[derive(Debug)]
pub struct RequestMessages {
    pub messages: Vec<Message>,
    pub system_prefix_len: usize,
}
#[derive(Debug, Default)]
pub struct RequestEdit {
    pub messages: Option<RequestMessages>,
    pub options: Option<ModelOptions>,
    /// A unique subset of the preceding view's unchanged host tool definitions.
    pub tools: Option<Vec<String>>,
    pub tool_choice: Option<ModelToolChoice>,
}
#[async_trait]
pub trait RequestHook: Send + Sync {
    /// Read-only/idempotent: selection/preparation and post-compaction previews can call again.
    async fn request(
        &self,
        input: RequestHookInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<Option<RequestEdit>>;
}
#[derive(Clone)]
pub(crate) struct RequestRegistration {
    pub hook: Registered<dyn RequestHook>,
    pub scope: RequestHookScope,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestChange {
    pub extension: String,
    pub hook: String,
    pub scope: RequestHookScope,
    pub messages_changed: bool,
    pub options_changed: bool,
    pub tools_changed: bool,
}
#[derive(Debug, Clone)]
pub(crate) struct PreparedRequestView {
    pub owner: uuid::Uuid,
    pub round: usize,
    pub request: ModelRequest,
    pub prefix: usize,
    pub changes: Vec<RequestChange>,
    pub pass: RequestHookPass,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestViewStage {
    Admission,
    Ready,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestViewRecord {
    pub round: usize,
    pub pass: RequestHookPass,
    pub stage: RequestViewStage,
    pub provider: String,
    pub model: String,
    pub source_messages: usize,
    pub messages: usize,
    pub system_prefix_len: usize,
    pub tools: Vec<String>,
    pub tool_choice: ModelToolChoice,
    pub changes: Vec<RequestChange>,
    pub input_count: TokenCount,
    pub effective_options: RequestOptionSummary,
}
pub(crate) fn apply_edit(
    request: &mut ModelRequest,
    prefix: &mut usize,
    edit: RequestEdit,
    scope: RequestHookScope,
) -> anyhow::Result<RequestChangeFlags> {
    let mut flags = RequestChangeFlags::default();
    if let Some(view) = edit.messages {
        anyhow::ensure!(
            view.system_prefix_len <= view.messages.len(),
            "request hook prefix exceeds its view"
        );
        crate::input_context::validate_prefix(&view.messages[..view.system_prefix_len])?;
        match scope {
            RequestHookScope::Conversation => anyhow::ensure!(
                view.system_prefix_len == *prefix
                    && view.messages.get(..*prefix) == request.messages.get(..*prefix),
                "conversation hook changed the protected runtime prefix"
            ),
            RequestHookScope::SystemAndConversation => anyhow::ensure!(
                *prefix == 0 || view.system_prefix_len > 0,
                "full request hook removed its declared system prefix"
            ),
        }
        size(&view.messages, MAX_REQUEST_VIEW_BYTES)?;
        flags.messages = request.messages != view.messages || *prefix != view.system_prefix_len;
        request.messages = view.messages;
        *prefix = view.system_prefix_len;
    }
    if let Some(options) = edit.options {
        size(&options, 64 * 1024)?;
        flags.options = request.options != options;
        request.options = options;
    }
    if let Some(names) = edit.tools {
        let mut unique = std::collections::HashSet::new();
        anyhow::ensure!(
            names.len() <= 1024
                && names
                    .iter()
                    .all(|n| unique.insert(n) && request.tools.iter().any(|t| t.name == *n)),
            "request hook tools must be a unique subset of the preceding host catalog"
        );
        let selected = names
            .iter()
            .map(|name| {
                request
                    .tools
                    .iter()
                    .find(|t| t.name == *name)
                    .expect("checked tool")
                    .clone()
            })
            .collect::<Vec<_>>();
        flags.tools = request.tools != selected;
        request.tools = selected;
    }
    if let Some(choice) = edit.tool_choice {
        flags.tools |= request.tool_choice != choice;
        request.tool_choice = choice;
    }
    size(request, MAX_REQUEST_VIEW_BYTES)?;
    Ok(flags)
}
#[derive(Default)]
pub(crate) struct RequestChangeFlags {
    pub messages: bool,
    pub options: bool,
    pub tools: bool,
}

/// Safe scalar observation; private stop strings and JSON schemas are not copied into history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestOptionSummary {
    pub reasoning_effort: Option<ReasoningEffort>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub seed: Option<i64>,
    pub stop_sequences: Option<usize>,
    pub response_format: Option<String>,
    pub complete: bool,
}
impl From<&ModelOptions> for RequestOptionSummary {
    fn from(options: &ModelOptions) -> Self {
        let ModelOptions {
            reasoning_effort,
            temperature,
            top_p,
            max_output_tokens,
            seed,
            stop,
            response_format,
        } = options;
        Self {
            reasoning_effort: *reasoning_effort,
            temperature: *temperature,
            top_p: *top_p,
            max_output_tokens: *max_output_tokens,
            seed: *seed,
            stop_sequences: stop.as_ref().map(Vec::len),
            response_format: response_format.as_ref().map(|f| {
                match f {
                    ResponseFormat::Text => "text",
                    ResponseFormat::JsonObject => "json_object",
                    ResponseFormat::JsonSchema { .. } => "json_schema",
                }
                .into()
            }),
            complete: stop.as_ref().is_none_or(Vec::is_empty)
                && !matches!(response_format, Some(ResponseFormat::JsonSchema { .. })),
        }
    }
}
