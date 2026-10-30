//! Canonical history and the independently projected messages sent to a model.
use crate::{
    model::{Message, MessageRole, ModelRequest},
    session::SessionContextSource,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelContextInfo {
    /// Requested coding effort, not proof of actual/billed reasoning work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<crate::model::ReasoningEffort>,
    pub source: Option<SessionContextSource>,
    pub initial_history_messages: usize,
    pub initial_context_messages: usize,
    pub requests: usize,
    pub last_request_messages: Option<usize>,
    pub last_input_token_estimate: Option<u64>,
    pub compactions: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub summary_attempts: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub summary_fallbacks: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_summary_checkpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_input_count: Option<crate::tokens::TokenCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_count: Option<crate::tokens::TokenCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_decision: Option<crate::context_policy::ContextDecision>,
}
#[derive(Debug, Clone)]
pub struct ExecutionInput {
    pub(crate) request: ModelRequest,
    pub(crate) history: Vec<Message>,
    pub(crate) system_prefix_len: usize,
    pub(crate) source: Option<SessionContextSource>,
    pub(crate) prepared_context: Option<serde_json::Value>,
    pub(crate) started: Option<tokio::time::Instant>,
}
impl ExecutionInput {
    pub fn from_request(request: ModelRequest) -> Self {
        let prefix = request
            .messages
            .iter()
            .take_while(|message| message.role == MessageRole::System)
            .count();
        Self {
            history: request.messages.clone(),
            request,
            system_prefix_len: prefix,
            source: None,
            prepared_context: None,
            started: None,
        }
    }
    /// Explicit host projection. Only the shared runtime instruction prefix is replaced
    /// by input-context providers; derived historical system markers are preserved.
    pub fn projected(
        request: ModelRequest,
        history: Vec<Message>,
        system_prefix_len: usize,
    ) -> Result<Self> {
        ensure!(
            system_prefix_len <= history.len() && system_prefix_len <= request.messages.len(),
            "instruction prefix exceeds input length"
        );
        ensure!(
            history
                .iter()
                .take_while(|message| message.role == MessageRole::System)
                .count()
                == system_prefix_len,
            "history must declare its complete runtime system prefix"
        );
        ensure!(
            history[..system_prefix_len] == request.messages[..system_prefix_len]
                && history[..system_prefix_len]
                    .iter()
                    .all(|message| message.role == MessageRole::System),
            "history and model context must share the declared system prefix"
        );
        Ok(Self {
            request,
            history,
            system_prefix_len,
            source: None,
            prepared_context: None,
            started: None,
        })
    }
    /// Read-only branch execution: request messages contain the runtime system prefix
    /// and fresh input, while the branch supplies recorded history and safe context.
    pub fn from_branch(
        mut request: ModelRequest,
        branch: &crate::session::SessionBranch<'_>,
    ) -> Result<Self> {
        let prefix = request
            .messages
            .iter()
            .take_while(|message| message.role == MessageRole::System)
            .count();
        let fresh = request.messages.split_off(prefix);
        let mut history = request.messages.clone();
        history.extend(branch.chat_messages());
        history.extend(fresh.clone());
        let projection = branch.model_context()?;
        let source = projection.source.clone();
        request.messages.extend(projection.into_messages());
        request.messages.extend(fresh);
        let mut input = Self::projected(request, history, prefix)?;
        input.source = Some(source);
        Ok(input)
    }
    pub fn from_run_session(
        mut request: ModelRequest,
        session: &crate::session::RunSession,
    ) -> Result<Self> {
        let prefix = request
            .messages
            .iter()
            .take_while(|message| message.role == MessageRole::System)
            .count();
        let fresh = request.messages.split_off(prefix);
        let mut history = request.messages.clone();
        history.extend(
            session
                .messages()
                .into_iter()
                .map(|entry| entry.message.clone()),
        );
        history.extend(fresh.clone());
        let (source, context) = session.source_and_context()?;
        request.messages.extend(context);
        request.messages.extend(fresh);
        let mut input = Self::projected(request, history, prefix)?;
        input.source = Some(source);
        Ok(input)
    }
    pub fn request(&self) -> &ModelRequest {
        &self.request
    }
    pub fn history(&self) -> &[Message] {
        &self.history
    }
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}
