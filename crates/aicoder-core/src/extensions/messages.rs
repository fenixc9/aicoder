//! Finalization of fresh canonical messages. History and provider projections are not inputs.
use super::*;
use crate::{CustomMessageMetadata, TurnExecutionContext, model::*};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
pub const MAX_MESSAGE_FINALIZATION_BYTES: usize = 2 * 1024 * 1024;
pub const MESSAGE_FINALIZATION_NAMESPACE: &str = "aicoder.message-finalization.v1";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageOrigin {
    Initial,
    NextTurn,
    IdleCustom,
    Steer,
    FollowUp,
    Deferred,
    Assistant,
    Tool,
    Startup,
    Boundary,
}
impl MessageOrigin {
    pub(crate) fn accepts(self, role: MessageRole) -> bool {
        match self {
            Self::Assistant => role == MessageRole::Assistant,
            Self::Tool => role == MessageRole::Tool,
            Self::Startup | Self::Boundary => role == MessageRole::System,
            Self::IdleCustom | Self::NextTurn | Self::Deferred => role == MessageRole::Custom,
            Self::Initial | Self::Steer | Self::FollowUp => {
                matches!(role, MessageRole::User | MessageRole::Custom)
            }
        }
    }
}
pub struct MessageHookInput<'a> {
    pub workspace: &'a Path,
    pub origin: MessageOrigin,
    pub round: Option<usize>,
    pub original: &'a Message,
    pub message: &'a Message,
    pub custom: Option<&'a CustomMessageMetadata>,
}
#[async_trait]
pub trait MessageHook: Send + Sync {
    async fn finalize(
        &self,
        input: MessageHookInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<Option<Message>>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageFinalizationReceipt {
    pub origin: MessageOrigin,
    pub role: MessageRole,
    pub round: Option<usize>,
    pub original_bytes: usize,
    pub effective_bytes: usize,
    pub changed: bool,
    pub changes: Vec<FinalizationChange>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageFinalizationRecord {
    Started { hooks: Vec<String> },
    Applied { receipt: MessageFinalizationReceipt },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizedMessage {
    pub message: Message,
    pub custom: Option<CustomMessageMetadata>,
    pub receipt: MessageFinalizationReceipt,
}
impl FinalizedMessage {
    pub fn encoded_bytes(&self) -> anyhow::Result<usize> {
        size(self, MAX_MESSAGE_FINALIZATION_BYTES + 64 * 1024)
    }
    pub(crate) fn receipt_entry(&self) -> anyhow::Result<crate::session::SessionEntryData> {
        receipt_entry(&self.receipt)
    }
    pub(crate) fn event(&self) -> crate::AgentRawEvent {
        crate::AgentRawEvent::MessageFinalized {
            message: Arc::new(self.message.clone()),
            custom: self.custom.clone(),
            origin: self.receipt.origin,
            changed: self.receipt.changed,
        }
    }
}
pub(crate) fn receipt_entry(
    receipt: &MessageFinalizationReceipt,
) -> anyhow::Result<crate::session::SessionEntryData> {
    size(receipt, 64 * 1024)?;
    Ok(crate::session::SessionEntryData::Extension {
        namespace: MESSAGE_FINALIZATION_NAMESPACE.into(),
        data: serde_json::to_value(MessageFinalizationRecord::Applied {
            receipt: receipt.clone(),
        })?,
    })
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizedInput {
    pub run_id: String,
    pub id: u64,
    pub kind: crate::RunInputKind,
    pub receipt: Option<InputReceipt>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageFinalizationCommit {
    pub index: usize,
    pub finalized: FinalizedMessage,
    pub input: Option<FinalizedInput>,
    pub assistant: Option<FinalizationRecord>,
}
impl MessageFinalizationCommit {
    pub fn session_entries(&self) -> anyhow::Result<Vec<crate::session::SessionEntryData>> {
        use crate::session::SessionEntryData as E;
        size(self, MAX_MESSAGE_FINALIZATION_BYTES + 128 * 1024)?;
        anyhow::ensure!(
            self.finalized.receipt.role == self.finalized.message.role
                && self
                    .finalized
                    .receipt
                    .origin
                    .accepts(self.finalized.message.role),
            "finalization receipt role/origin mismatch"
        );
        anyhow::ensure!(
            self.finalized.receipt.effective_bytes
                == size(&self.finalized.message, MAX_MESSAGE_FINALIZATION_BYTES)?,
            "finalization receipt size mismatch"
        );
        anyhow::ensure!(
            self.finalized.message.role != MessageRole::System,
            "sourced system context requires a boundary transaction"
        );
        let mut entries = vec![self.finalized.receipt_entry()?];
        if let Some(record) = &self.assistant {
            entries.push(E::Extension {
                namespace: FINALIZATION_NAMESPACE.into(),
                data: serde_json::to_value(record)?,
            });
        }
        if let Some(input) = &self.input {
            if let Some(receipt) = &input.receipt {
                entries.push(E::Extension {
                    namespace: INPUT_HOOK_NAMESPACE.into(),
                    data: serde_json::to_value(InputHookRecord::Prepared {
                        receipt: receipt.for_record(),
                    })?,
                });
            }
            let namespace = if self.finalized.custom.is_some() {
                "aicoder.custom-delivery.v1"
            } else {
                "aicoder.run.input"
            };
            anyhow::ensure!(
                matches!(
                    self.finalized.message.role,
                    MessageRole::User | MessageRole::Custom
                ),
                "finalized input has wrong role"
            );
            anyhow::ensure!(
                input.kind != crate::RunInputKind::Context || self.finalized.custom.is_some(),
                "context input requires custom metadata"
            );
            let mut data =
                serde_json::json!({"run_id":input.run_id,"id":input.id,"kind":input.kind});
            if let Some(custom) = &self.finalized.custom {
                data["source"] = serde_json::json!(custom.source);
                data["custom_type"] = serde_json::json!(custom.custom_type);
            }
            entries.push(E::Extension {
                namespace: namespace.into(),
                data,
            });
        }
        entries.push(match &self.finalized.custom {
            Some(metadata) => E::CustomMessage {
                custom: crate::CustomMessage::from_canonical(
                    metadata.clone(),
                    self.finalized.message.clone(),
                )?,
            },
            None => {
                anyhow::ensure!(
                    self.finalized.message.role != MessageRole::Custom,
                    "custom finalization lacks metadata"
                );
                E::Message {
                    message: self.finalized.message.clone(),
                }
            }
        });
        Ok(entries)
    }
}
#[derive(Clone)]
pub(crate) struct MessageFinalizers {
    workspace: PathBuf,
    hooks: Vec<Registered<dyn MessageHook>>,
}
impl MessageFinalizers {
    pub fn new(workspace: &Path, hooks: Vec<Registered<dyn MessageHook>>) -> Self {
        Self {
            workspace: workspace.into(),
            hooks,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
    pub fn started(&self) -> MessageFinalizationRecord {
        MessageFinalizationRecord::Started {
            hooks: self
                .hooks
                .iter()
                .map(|h| format!("{}/{}", h.owner, h.spec.id))
                .collect(),
        }
    }
    pub async fn prepare(
        &self,
        original: &Message,
        custom: Option<&CustomMessageMetadata>,
        origin: MessageOrigin,
        round: Option<usize>,
        context: &TurnExecutionContext,
    ) -> anyhow::Result<Option<FinalizedMessage>> {
        if self.is_empty() {
            return Ok(None);
        }
        anyhow::ensure!(
            origin.accepts(original.role),
            "message finalization origin/role mismatch"
        );
        let original_bytes = size(original, MAX_MESSAGE_FINALIZATION_BYTES)?;
        let mut message = original.clone();
        let mut changes = Vec::new();
        let mut aggregate = 0usize;
        let work = async {
            for h in &self.hooks {
                if let Some(next) = invoke(&h.owner, &h.spec, context.ui(), |c| {
                    h.callback.finalize(
                        MessageHookInput {
                            workspace: &self.workspace,
                            origin,
                            round,
                            original,
                            message: &message,
                            custom,
                        },
                        c,
                    )
                })
                .await?
                {
                    validate(original, &next, custom)?;
                    aggregate =
                        aggregate.saturating_add(size(&next, MAX_MESSAGE_FINALIZATION_BYTES)?);
                    anyhow::ensure!(
                        aggregate <= 4 * MAX_MESSAGE_FINALIZATION_BYTES,
                        "message finalization chain exceeds aggregate budget"
                    );
                    changes.push(FinalizationChange {
                        extension: h.owner.clone(),
                        hook: h.spec.id.clone(),
                        changed: next != message,
                    });
                    message = next;
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::select! {biased; cancelled=context.cancelled()=>return Err(cancelled.into()),result=tokio::time::timeout(Duration::from_secs(30),work)=>result.map_err(|_|anyhow::anyhow!("message finalization phase timed out"))??}
        let receipt = MessageFinalizationReceipt {
            origin,
            role: original.role,
            round,
            original_bytes,
            effective_bytes: size(&message, MAX_MESSAGE_FINALIZATION_BYTES)?,
            changed: message != *original,
            changes,
        };
        size(&receipt, 64 * 1024)?;
        let result = FinalizedMessage {
            message,
            custom: custom.cloned(),
            receipt,
        };
        result.encoded_bytes()?;
        Ok(Some(result))
    }
}
fn validate(
    original: &Message,
    next: &Message,
    custom: Option<&CustomMessageMetadata>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        next.role == original.role && !next.content.is_empty(),
        "message finalization must preserve role and nonempty content"
    );
    match original.role {
        MessageRole::Assistant => {
            validate_assistant_edit(original, next, MAX_MESSAGE_FINALIZATION_BYTES)?
        }
        MessageRole::User => {
            crate::AgentInput::try_from(next.clone())?;
        }
        MessageRole::Custom => {
            let metadata = custom
                .ok_or_else(|| anyhow::anyhow!("custom finalization lacks source metadata"))?;
            crate::CustomMessage::from_canonical(metadata.clone(), next.clone())?;
        }
        MessageRole::System => {
            anyhow::ensure!(
                explicit_context_message(original) && explicit_context_message(next),
                "system finalization requires explicit sourced context"
            );
            let old = original.text_content();
            let new = next.text_content();
            anyhow::ensure!(
                old.split_once("]\n").map(|v| v.0) == new.split_once("]\n").map(|v| v.0),
                "message finalization changed context provenance"
            );
        }
        MessageRole::Tool => {
            anyhow::ensure!(
                original.content.len() == next.content.len(),
                "tool finalization changed typed media"
            );
            for (old, new) in original.content.iter().zip(&next.content) {
                match (old, new) {
                    (
                        ContentBlock::ToolResult {
                            call_id: a,
                            is_error: x,
                            content: before,
                        },
                        ContentBlock::ToolResult {
                            call_id: b,
                            is_error: y,
                            content: after,
                        },
                    ) => {
                        anyhow::ensure!(
                            a == b && x == y,
                            "tool finalization changed identity or delivery status"
                        );
                        let key = if *x { "error" } else { "output" };
                        let mut lhs = before.clone();
                        let mut rhs = after.clone();
                        let l = lhs
                            .as_object_mut()
                            .ok_or_else(|| anyhow::anyhow!("invalid original tool envelope"))?;
                        let r = rhs
                            .as_object_mut()
                            .ok_or_else(|| anyhow::anyhow!("invalid finalized tool envelope"))?;
                        l.remove(key);
                        r.remove(key);
                        anyhow::ensure!(
                            lhs == rhs && after.get(key).is_some(),
                            "tool finalization changed execution/capture/artifact fields"
                        );
                        if *x {
                            let error = &after[key];
                            anyhow::ensure!(
                                error["details"] == before[key]["details"]
                                    && error["code"] == before[key]["code"]
                                    && error["message"].is_string()
                                    && error.as_object().is_some_and(|o| o.keys().all(
                                        |k| matches!(k.as_str(), "code" | "message" | "details")
                                    )),
                                "tool finalization changed error class or envelope"
                            );
                        }
                    }
                    _ => anyhow::ensure!(
                        old == new,
                        "tool finalization changed typed media or protected blocks"
                    ),
                }
            }
        }
    }
    size(next, MAX_MESSAGE_FINALIZATION_BYTES)?;
    Ok(())
}
