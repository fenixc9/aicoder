//! Branch-local model-view edits; canonical messages and runtime authority remain separate.
use super::*;
use crate::model::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
pub const CONTEXT_EDIT_NAMESPACE: &str = "aicoder.context.edit.v1";
pub const MAX_CONTEXT_EDIT_BYTES: usize = 64 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "content",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ContextReplacement {
    Omit,
    Restore,
    Text(String),
    Blocks(Vec<ContentBlock>),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextEditRecord {
    pub version: u32,
    pub extension: String,
    pub hook: String,
    pub target_id: String,
    pub replacement: ContextReplacement,
}
impl ContextEditRecord {
    /// Validate against a real canonical message, never a private archive descriptor.
    pub fn validate_replacement(&self, original: &Message) -> Result<()> {
        self.replacement_message(original).map(|_| ())
    }
    pub fn data(&self) -> SessionEntryData {
        SessionEntryData::Extension {
            namespace: CONTEXT_EDIT_NAMESPACE.into(),
            data: serde_json::to_value(self).expect("context edit"),
        }
    }
    pub fn validate<'a>(
        &self,
        target: &SessionEntry,
        ancestors: impl Iterator<Item = &'a str>,
    ) -> Result<()> {
        ensure!(self.version == 1, "unsupported context edit version");
        crate::extensions::name(&self.extension)?;
        crate::extensions::name(&self.hook)?;
        ensure!(
            target.id == self.target_id && ancestors.into_iter().any(|id| id == self.target_id),
            "context edit target is not on the active ancestry"
        );
        let message = target
            .data
            .as_message()
            .ok_or_else(|| anyhow::anyhow!("context edit target has no canonical message"))?;
        ensure!(
            message.role != MessageRole::System,
            "runtime/sourced system context is not editable"
        );
        crate::extensions::size(self, MAX_CONTEXT_EDIT_BYTES)?;
        if let ContextReplacement::Blocks(blocks) = &self.replacement {
            ensure!(
                !blocks.is_empty(),
                "use omit for an empty context replacement"
            );
        }
        Ok(())
    }
    pub(crate) fn replacement_message(&self, original: &Message) -> Result<Option<Message>> {
        let next = match &self.replacement {
            ContextReplacement::Omit => return Ok(None),
            ContextReplacement::Restore => return Ok(Some(original.clone())),
            ContextReplacement::Blocks(content) => Message {
                role: original.role,
                content: content.clone(),
            },
            ContextReplacement::Text(text) => match original.role {
                MessageRole::User => Message::text(original.role, text),
                MessageRole::Custom => {
                    let label = original
                        .content
                        .first()
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("custom context lacks its source"))?;
                    Message {
                        role: original.role,
                        content: vec![label, ContentBlock::Text { text: text.clone() }],
                    }
                }
                MessageRole::Assistant => {
                    let mut content = original
                        .content
                        .iter()
                        .filter(|b| {
                            !matches!(
                                b,
                                ContentBlock::Text { .. }
                                    | ContentBlock::Reasoning { .. }
                                    | ContentBlock::Image { .. }
                            )
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    content.push(ContentBlock::Text { text: text.clone() });
                    Message {
                        role: original.role,
                        content,
                    }
                }
                MessageRole::Tool => {
                    let mut message = original.clone();
                    message
                        .content
                        .retain(|b| !matches!(b, ContentBlock::Image { .. }));
                    for block in &mut message.content {
                        if let ContentBlock::ToolResult {
                            content, is_error, ..
                        } = block
                        {
                            if !content.is_object() {
                                *content = serde_json::json!({});
                            }
                            if *is_error {
                                if !content["error"].is_object() {
                                    content["error"] = serde_json::json!({});
                                }
                                content["error"]["message"] = serde_json::json!(text);
                            } else {
                                content["output"] = serde_json::json!(text);
                            }
                        }
                    }
                    message
                }
                MessageRole::System => anyhow::bail!("system context cannot be replaced"),
            },
        };
        ensure!(
            next.role == original.role && !next.content.is_empty(),
            "invalid context replacement role/content"
        );
        let images = original
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::Image { .. }))
            .collect::<Vec<_>>();
        ensure!(
            next.content
                .iter()
                .filter(|b| matches!(b, ContentBlock::Image { .. }))
                .all(|b| images.contains(&b)),
            "context edits cannot add image IO/reference authority"
        );
        match original.role {
            MessageRole::User => {
                crate::AgentInput::try_from(next.clone())?;
            }
            MessageRole::Custom => {
                ensure!(
                    next.content.first() == original.content.first(),
                    "context edit changed custom source"
                );
                crate::agent_prompt::validate_custom_message(&next)?;
            }
            MessageRole::Assistant => {
                let protected = |m: &Message| {
                    m.content
                        .iter()
                        .filter(|b| {
                            matches!(
                                b,
                                ContentBlock::ToolCall { .. } | ContentBlock::ProviderData { .. }
                            )
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                };
                ensure!(
                    protected(original) == protected(&next),
                    "context edit changed a protected call or provider state"
                );
                ensure!(
                    next.content.iter().all(|b| matches!(
                        b,
                        ContentBlock::Text { .. }
                            | ContentBlock::Reasoning { .. }
                            | ContentBlock::ToolCall { .. }
                            | ContentBlock::ProviderData { .. }
                            | ContentBlock::Image { .. }
                    )),
                    "invalid assistant context blocks"
                );
            }
            MessageRole::Tool => {
                let old = original
                    .content
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolResult { call_id, .. } = b {
                            Some(call_id)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                let new = next
                    .content
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolResult { call_id, .. } = b {
                            Some(call_id)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                ensure!(old == new, "context edit changed tool result identity");
                for (old, new) in original
                    .content
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
                    .zip(
                        next.content
                            .iter()
                            .filter(|b| matches!(b, ContentBlock::ToolResult { .. })),
                    )
                {
                    if let (
                        ContentBlock::ToolResult { is_error: a, .. },
                        ContentBlock::ToolResult { is_error: b, .. },
                    ) = (old, new)
                    {
                        ensure!(a == b, "context edit changed completed tool status");
                    }
                }
                ensure!(
                    next.content.iter().all(|b| matches!(
                        b,
                        ContentBlock::ToolResult { .. } | ContentBlock::Image { .. }
                    )),
                    "invalid tool context blocks"
                );
            }
            MessageRole::System => anyhow::bail!("system context cannot be replaced"),
        }
        // Reference filtering is model-facing data; it never resolves URLs or changes raw artifacts.
        crate::extensions::size(&next, MAX_CONTEXT_EDIT_BYTES)?;
        Ok(Some(next))
    }
}
pub(crate) fn apply_context_edits<'a>(
    entries: &[&'a SessionEntry],
    view: &mut SessionContext<'a>,
) -> Result<()> {
    // Ordinary sessions pay no per-record index allocation for an unused feature.
    let records = entries
        .iter()
        .filter_map(|entry| match &entry.data {
            SessionEntryData::Extension { namespace, data }
                if namespace == CONTEXT_EDIT_NAMESPACE =>
            {
                Some(
                    serde_json::from_value::<ContextEditRecord>(data.clone())
                        .map(|record| (*entry, record)),
                )
            }
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?;
    if records.is_empty() {
        return Ok(());
    }
    let by_id = entries
        .iter()
        .map(|e| (e.id.as_str(), *e))
        .collect::<HashMap<_, _>>();
    let mut edits = HashMap::new();
    for (entry, record) in records {
        let target = by_id
            .get(record.target_id.as_str())
            .ok_or_else(|| anyhow::anyhow!("context edit target missing"))?;
        edits.insert(target.id.as_str(), (entry, record));
    }
    // Resolve whole canonical records first. A tool record may contribute multiple projected
    // results; validating a whole replacement against an individual result would lose identity.
    let mut replacements = HashMap::new();
    let mut omitted = HashSet::new();
    for (id, (_, record)) in &edits {
        let original = by_id[id]
            .data
            .as_message()
            .ok_or_else(|| anyhow::anyhow!("context edit has no message"))?;
        match record.replacement_message(original)? {
            Some(message) => {
                replacements.insert(*id, message);
            }
            None => {
                omitted.insert(*id);
            }
        }
    }
    // Build adjacency from actual projection units, never global call IDs (which may repeat
    // in a later turn). A multi-result raw record connects every unit it contributes to.
    let mut adjacent: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut owner = None;
    for (message, origin) in view.messages.iter().zip(&view.origins) {
        if message.role != MessageRole::Tool {
            owner = if message.tool_calls().next().is_some() {
                origin.recorded_entry_id()
            } else {
                None
            };
        } else if let (Some(owner), Some(result)) = (owner, origin.recorded_entry_id()) {
            adjacent.entry(owner).or_default().push(result);
            adjacent.entry(result).or_default().push(owner);
        }
    }
    let mut pending = omitted.iter().copied().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        if let Some(neighbors) = adjacent.get(id) {
            for neighbor in neighbors {
                if omitted.insert(*neighbor) {
                    pending.push(*neighbor);
                }
            }
        }
    }
    let mut messages = Vec::with_capacity(view.messages.len());
    let mut origins = Vec::with_capacity(view.origins.len());
    for (message, origin) in std::mem::take(&mut view.messages)
        .into_iter()
        .zip(std::mem::take(&mut view.origins))
    {
        let id = match &origin {
            ContextOrigin::Recorded { entry_id } => Some(*entry_id),
            ContextOrigin::RecoveredToolResult { call_entry_id, .. } => Some(*call_entry_id),
            _ => None,
        };
        if id.is_some_and(|id| omitted.contains(id)) {
            continue;
        }
        if let ContextOrigin::Recorded { entry_id: id } = &origin
            && let Some(replacement) = replacements.get(id)
        {
            let mut replacement = replacement.clone();
            if message.role == MessageRole::Tool {
                let ids = message
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolResult { call_id, .. } => Some(call_id),
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                replacement.content.retain(|b| match b {
                    ContentBlock::ToolResult { call_id, .. } => ids.contains(call_id),
                    ContentBlock::Image { .. } => true,
                    _ => false,
                });
            }
            messages.push(replacement);
            origins.push(ContextOrigin::Edited {
                entry_id: id,
                edit_entry_id: &edits[id].0.id,
            });
        } else {
            messages.push(message);
            origins.push(origin);
        }
    }
    view.messages = messages;
    view.origins = origins;
    Ok(())
}
