//! Assistant finalization after paid provider completion, before verification/tool dispatch.
use super::*;
use crate::model::*;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
pub const FINALIZATION_NAMESPACE: &str = "aicoder.assistant-finalization.v1";
pub const MAX_FINAL_MESSAGE_BYTES: usize = 1024 * 1024;
/// The paid response and the current ordered edit. Neither reference grants model/tool authority.
pub struct AssistantMessageInput<'a> {
    pub workspace: &'a Path,
    pub round: usize,
    pub original: &'a Message,
    pub message: &'a Message,
    pub usage: &'a ModelUsage,
    pub stop_reason: &'a StopReason,
}
#[async_trait]
/// Finalize assistant text/visible reasoning before canonical persistence and owned dispatch.
/// Returning `None` preserves the current message. All other blocks must remain identical.
pub trait AssistantMessageHook: Send + Sync {
    async fn finalize(
        &self,
        input: AssistantMessageInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<Option<Message>>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizationChange {
    pub extension: String,
    pub hook: String,
    pub changed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FinalizationRecord {
    Started {
        hooks: Vec<String>,
    },
    Applied {
        round: usize,
        original_bytes: usize,
        effective_bytes: usize,
        changes: Vec<FinalizationChange>,
    },
}
pub(crate) fn validate_edit(original: &Message, next: &Message) -> anyhow::Result<()> {
    validate_assistant_edit(original, next, MAX_FINAL_MESSAGE_BYTES)
}
pub(crate) fn validate_assistant_edit(
    original: &Message,
    next: &Message,
    limit: usize,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        next.role == MessageRole::Assistant && !next.content.is_empty(),
        "assistant finalization must preserve role and nonempty content"
    );
    let protected = |m: &Message| {
        m.content
            .iter()
            .filter(|b| {
                !matches!(
                    b,
                    ContentBlock::Text { .. } | ContentBlock::Reasoning { .. }
                )
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    anyhow::ensure!(
        protected(original) == protected(next),
        "assistant finalization changed protected action or provider state"
    );
    size(next, limit)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visible_edits_preserve_opaque_provider_state_and_exact_tool_actions() {
        let original = Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::ProviderData {
                    provider: "fixture".into(),
                    data: serde_json::json!({"signature":"immutable"}),
                },
                ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "call".into(),
                        name: "write".into(),
                        arguments: serde_json::json!({"path":"owned"}),
                    },
                },
            ],
        };
        let mut edited = original.clone();
        edited.content.push(ContentBlock::Reasoning {
            text: "visible".into(),
        });
        validate_edit(&original, &edited).unwrap();
        let mut altered = edited.clone();
        if let ContentBlock::ProviderData { data, .. } = &mut altered.content[0] {
            *data = serde_json::json!({"signature":"changed"});
        }
        assert!(validate_edit(&original, &altered).is_err());
        if let ContentBlock::ToolCall { call } = &mut edited.content[1] {
            call.arguments = serde_json::json!({"path":"unapproved"});
        }
        assert!(validate_edit(&original, &edited).is_err());
    }
}
