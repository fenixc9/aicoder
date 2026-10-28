//! Context-window budgeting and pluggable compaction policies.

use anyhow::{Result, bail, ensure};
use async_trait::async_trait;
use serde::Serialize;

use crate::types::{ChatMessage, Role, Tool, Usage};

#[derive(Debug, Clone, Copy)]
pub struct ContextCompactionInput<'a> {
    pub model: &'a str,
    pub messages: &'a [ChatMessage],
    pub tools: Option<&'a [Tool]>,
    pub max_output_tokens: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ContextCompaction {
    pub messages: Vec<ChatMessage>,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
    pub removed_messages: usize,
    /// Usage consumed by semantic/model-backed compactors. Deterministic compactors leave this at zero.
    pub usage: Usage,
}

#[async_trait]
pub trait ContextCompactor: Send + Sync {
    fn name(&self) -> &'static str;

    fn estimate_tokens(&self, input: ContextCompactionInput<'_>) -> usize;

    /// Maximum estimated input tokens the compacted request may contain.
    fn target_tokens(&self, input: ContextCompactionInput<'_>) -> usize;

    async fn compact(&self, input: ContextCompactionInput<'_>) -> Result<ContextCompaction>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextWindowConfig {
    /// Complete model context window, including the reserved completion budget.
    pub max_context_tokens: usize,
    /// Completion capacity kept when the request does not specify a larger `max_tokens` value.
    pub reserved_output_tokens: usize,
    /// Target capacity for recent complete message units retained during pruning.
    pub preserve_recent_tokens: usize,
}

impl ContextWindowConfig {
    pub fn validate(self) -> Result<Self> {
        ensure!(
            self.max_context_tokens > self.reserved_output_tokens,
            "max_context_tokens must exceed reserved_output_tokens"
        );
        ensure!(
            self.preserve_recent_tokens < self.max_context_tokens - self.reserved_output_tokens,
            "preserve_recent_tokens must fit inside the input context budget"
        );
        Ok(self)
    }
}

/// Deterministically removes the oldest complete message units.
///
/// This policy never summarizes untrusted content or splits an assistant tool-call message from
/// its following tool results. The newest unit is always retained; additional recent units are kept
/// while they fit `preserve_recent_tokens`. It inserts a fixed marker and keeps the canonical
/// transcript in the executor unchanged. A semantic summarizer can implement `ContextCompactor`
/// without changing the execution loop.
#[derive(Debug, Clone)]
pub struct PruningContextCompactor {
    config: ContextWindowConfig,
}

impl PruningContextCompactor {
    pub fn new(config: ContextWindowConfig) -> Result<Self> {
        Ok(Self {
            config: config.validate()?,
        })
    }

    pub fn config(&self) -> ContextWindowConfig {
        self.config
    }

    fn input_budget(&self, max_output_tokens: Option<i32>) -> usize {
        let requested_output = max_output_tokens
            .and_then(|tokens| usize::try_from(tokens).ok())
            .unwrap_or_default();
        self.config
            .max_context_tokens
            .saturating_sub(requested_output.max(self.config.reserved_output_tokens))
    }
}

#[async_trait]
impl ContextCompactor for PruningContextCompactor {
    fn name(&self) -> &'static str {
        "prune_oldest"
    }

    fn estimate_tokens(&self, input: ContextCompactionInput<'_>) -> usize {
        estimate_context_tokens(input.messages, input.tools)
    }

    fn target_tokens(&self, input: ContextCompactionInput<'_>) -> usize {
        self.input_budget(input.max_output_tokens)
    }

    async fn compact(&self, input: ContextCompactionInput<'_>) -> Result<ContextCompaction> {
        let estimated_tokens_before = self.estimate_tokens(input);
        let target_tokens = self.target_tokens(input);
        ensure!(
            target_tokens > 0,
            "Model output reservation leaves no input context budget"
        );
        if estimated_tokens_before <= target_tokens {
            return Ok(ContextCompaction {
                messages: input.messages.to_vec(),
                estimated_tokens_before,
                estimated_tokens_after: estimated_tokens_before,
                removed_messages: 0,
                usage: Usage::default(),
            });
        }

        let units = message_units(input.messages);
        let mut mandatory = vec![false; units.len()];
        let mut recent_tokens = 0_usize;
        let mut retained_recent_unit = false;
        let mut recent_window_closed = false;
        for (index, unit) in units.iter().enumerate().rev() {
            if !unit.removable {
                mandatory[index] = true;
                continue;
            }
            let unit_tokens = estimate_messages(&input.messages[unit.start..unit.end]);
            if !retained_recent_unit
                || (!recent_window_closed
                    && recent_tokens.saturating_add(unit_tokens)
                        <= self.config.preserve_recent_tokens)
            {
                mandatory[index] = true;
                retained_recent_unit = true;
                recent_tokens += unit_tokens;
            } else {
                recent_window_closed = true;
            }
        }

        let mut removed = vec![false; units.len()];
        for (index, unit) in units.iter().enumerate() {
            if !unit.removable || mandatory[index] {
                continue;
            }
            removed[index] = true;
            let messages = rebuild_messages(input.messages, &units, &removed);
            let estimated_tokens_after = estimate_context_tokens(&messages, input.tools);
            if estimated_tokens_after <= target_tokens {
                return Ok(ContextCompaction {
                    removed_messages: removed_message_count(&units, &removed),
                    messages,
                    estimated_tokens_before,
                    estimated_tokens_after,
                    usage: Usage::default(),
                });
            }
        }

        let messages = rebuild_messages(input.messages, &units, &removed);
        let estimated_tokens_after = estimate_context_tokens(&messages, input.tools);
        bail!(
            "Context cannot fit the estimated {target_tokens}-token input budget without removing system or recent messages (estimated {estimated_tokens_after} tokens after pruning)"
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct MessageUnit {
    start: usize,
    end: usize,
    removable: bool,
}

fn message_units(messages: &[ChatMessage]) -> Vec<MessageUnit> {
    let mut units = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let start = index;
        let removable = messages[index].role != Role::System;
        index += 1;
        if messages[start].role == Role::Assistant
            && messages[start]
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty())
        {
            while index < messages.len() && messages[index].role == Role::Tool {
                index += 1;
            }
        }
        units.push(MessageUnit {
            start,
            end: index,
            removable,
        });
    }
    units
}

fn rebuild_messages(
    original: &[ChatMessage],
    units: &[MessageUnit],
    removed: &[bool],
) -> Vec<ChatMessage> {
    let removed_messages = removed_message_count(units, removed);
    let mut messages = Vec::with_capacity(original.len() - removed_messages + 1);
    let mut marker_inserted = false;
    for (unit, is_removed) in units.iter().zip(removed) {
        if *is_removed {
            if !marker_inserted {
                messages.push(compaction_marker(removed_messages));
                marker_inserted = true;
            }
            continue;
        }
        messages.extend_from_slice(&original[unit.start..unit.end]);
    }
    messages
}

fn removed_message_count(units: &[MessageUnit], removed: &[bool]) -> usize {
    units
        .iter()
        .zip(removed)
        .filter(|(_, removed)| **removed)
        .map(|(unit, _)| unit.end - unit.start)
        .sum()
}

fn compaction_marker(removed_messages: usize) -> ChatMessage {
    ChatMessage {
        role: Role::System,
        content: Some(format!(
            "[Context compaction removed {removed_messages} older messages. Reinspect the workspace when earlier details are needed.]"
        )),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

/// Conservative model-independent estimate. Provider-specific compactors can replace it with an
/// exact tokenizer while preserving the same executor contract.
pub fn estimate_context_tokens(messages: &[ChatMessage], tools: Option<&[Tool]>) -> usize {
    estimate_messages(messages) + tools.map_or(0, estimate_serialized) + 16
}

fn estimate_messages(messages: &[ChatMessage]) -> usize {
    messages.iter().map(estimate_serialized).sum::<usize>() + messages.len() * 4
}

fn estimate_serialized<T: Serialize + ?Sized>(value: &T) -> usize {
    let encoded = serde_json::to_string(value).unwrap_or_default();
    let (ascii, non_ascii): (usize, usize) =
        encoded
            .chars()
            .fold((0_usize, 0_usize), |(ascii, non_ascii), character| {
                if character.is_ascii() {
                    (ascii + 1, non_ascii)
                } else {
                    (ascii, non_ascii + 1)
                }
            });
    ascii.div_ceil(4) + non_ascii * 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FunctionCall, ToolCall, ToolType};

    fn message(role: Role, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: Some(content.to_string()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[tokio::test]
    async fn pruning_preserves_system_recent_context_and_tool_units() {
        let mut assistant = message(Role::Assistant, "calling tool");
        assistant.tool_calls = Some(vec![ToolCall {
            id: "call-1".to_string(),
            tool_type: ToolType::Function,
            function: FunctionCall {
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
            },
        }]);
        let messages = vec![
            message(Role::System, "system"),
            message(Role::User, &"old request ".repeat(80)),
            assistant,
            message(Role::Tool, &"old output ".repeat(80)),
            message(Role::User, "latest request"),
        ];
        let compactor = PruningContextCompactor::new(ContextWindowConfig {
            max_context_tokens: 180,
            reserved_output_tokens: 40,
            preserve_recent_tokens: 20,
        })
        .unwrap();
        let input = ContextCompactionInput {
            model: "test",
            messages: &messages,
            tools: None,
            max_output_tokens: None,
        };

        let compacted = compactor.compact(input).await.unwrap();

        assert!(compacted.removed_messages >= 3);
        assert_eq!(compacted.messages[0].role, Role::System);
        assert!(
            compacted
                .messages
                .iter()
                .any(|message| message.content.as_deref() == Some("latest request"))
        );
        assert!(!compacted.messages.iter().any(|message| {
            message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty())
        }));
        assert!(
            !compacted
                .messages
                .iter()
                .any(|message| message.role == Role::Tool)
        );
        assert!(compacted.estimated_tokens_after <= compactor.target_tokens(input));
    }

    #[tokio::test]
    async fn pruning_refuses_to_remove_oversized_recent_context() {
        let messages = vec![message(Role::User, &"current request ".repeat(200))];
        let compactor = PruningContextCompactor::new(ContextWindowConfig {
            max_context_tokens: 100,
            reserved_output_tokens: 20,
            preserve_recent_tokens: 40,
        })
        .unwrap();

        let error = compactor
            .compact(ContextCompactionInput {
                model: "test",
                messages: &messages,
                tools: None,
                max_output_tokens: None,
            })
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("without removing system or recent")
        );
    }
}
