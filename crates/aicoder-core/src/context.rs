//! Context-window budgeting and pluggable compaction policies.

use anyhow::{Result, bail, ensure};
use async_trait::async_trait;

use crate::model::{Message, MessageRole, ModelUsage, ToolDefinition};

#[derive(Debug, Clone, Copy)]
pub struct ContextCompactionInput<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub tools: Option<&'a [ToolDefinition]>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ContextCompaction {
    pub messages: Vec<Message>,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
    pub removed_messages: usize,
    /// ModelUsage consumed by semantic/model-backed compactors. Deterministic compactors leave this at zero.
    pub usage: ModelUsage,
}

#[async_trait]
pub trait ContextCompactor: Send + Sync {
    fn name(&self) -> &'static str;
    /// Built-ins honor counted targets. Legacy implementations retain their own trigger estimate.
    fn supports_counting(&self) -> bool {
        false
    }
    async fn compact_counted(
        &self,
        request: crate::summary::CompactionRequest<'_>,
        measure: ContextMeasure,
        context: crate::TurnExecutionContext,
    ) -> Result<crate::summary::RecordedCompaction> {
        let mut recorded = self.compact_recorded(request, context).await?;
        {
            recorded.compaction.estimated_tokens_before = measure
                .count(request.input.messages, request.input.tools)
                .tokens
                .min(usize::MAX as u64)
                as usize;
            recorded.compaction.estimated_tokens_after = measure
                .count(&recorded.compaction.messages, request.input.tools)
                .tokens
                .min(usize::MAX as u64)
                as usize;
        }
        Ok(recorded)
    }

    /// Whether compaction may consume model tokens. Unknown implementations are accounted conservatively.
    fn uses_model(&self) -> bool {
        true
    }

    fn manages_budget(&self) -> bool {
        false
    }
    fn persistent_summaries(&self) -> bool {
        false
    }
    async fn compact_recorded(
        &self,
        request: crate::summary::CompactionRequest<'_>,
        context: crate::TurnExecutionContext,
    ) -> Result<crate::summary::RecordedCompaction> {
        crate::summary::legacy(self, request, context).await
    }
    fn estimate_tokens(&self, input: ContextCompactionInput<'_>) -> usize;

    /// Maximum estimated input tokens the compacted request may contain.
    fn target_tokens(&self, input: ContextCompactionInput<'_>) -> usize;

    async fn compact(&self, input: ContextCompactionInput<'_>) -> Result<ContextCompaction>;
}

/// Shared request measurement and the host policy's selected input target.
#[derive(Clone)]
pub struct ContextMeasure {
    pub counting: crate::tokens::TokenCounting,
    pub spec: crate::model::ModelSpec,
    pub response_format: Option<crate::model::ResponseFormat>,
    pub tool_choice: crate::model::ModelToolChoice,
    pub target_tokens: usize,
}
impl ContextMeasure {
    pub fn count(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDefinition]>,
    ) -> crate::tokens::TokenCount {
        let adapted = crate::tokens::model_messages(messages, &self.spec);
        self.counting.count(crate::tokens::TokenInput {
            provider: &self.spec.provider,
            model: &self.spec.id,
            tokenizer: self.spec.tokenizer.as_deref(),
            content: crate::tokens::TokenContent::Request {
                messages: &adapted,
                tools,
                response_format: self.response_format.as_ref(),
                tool_choice: Some(&self.tool_choice),
            },
        })
    }
    pub(crate) fn unit_cost(&self, messages: &[Message]) -> usize {
        let adapted = crate::tokens::model_messages(messages, &self.spec);
        let count = |messages| {
            self.counting
                .count(crate::tokens::TokenInput {
                    provider: &self.spec.provider,
                    model: &self.spec.id,
                    tokenizer: self.spec.tokenizer.as_deref(),
                    content: crate::tokens::TokenContent::Request {
                        messages,
                        tools: None,
                        response_format: None,
                        tool_choice: None,
                    },
                })
                .tokens
        };
        count(&adapted)
            .saturating_sub(count(&[]))
            .min(usize::MAX as u64) as usize
    }
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

    fn prune_counted(
        &self,
        input: ContextCompactionInput<'_>,
        measure: &ContextMeasure,
    ) -> Result<ContextCompaction> {
        let before = measure
            .count(input.messages, input.tools)
            .tokens
            .min(usize::MAX as u64) as usize;
        let target = measure.target_tokens;
        ensure!(target > 0, "no input capacity");
        if before <= target {
            return Ok(ContextCompaction {
                messages: input.messages.to_vec(),
                estimated_tokens_before: before,
                estimated_tokens_after: before,
                removed_messages: 0,
                usage: ModelUsage::zero(),
            });
        }
        let units = message_units(input.messages);
        let mut mandatory = vec![false; units.len()];
        let mut recent = 0usize;
        let mut newest = true;
        let mut closed = false;
        for (index, unit) in units.iter().enumerate().rev() {
            if !unit.removable {
                mandatory[index] = true;
                continue;
            }
            let cost = measure.unit_cost(&input.messages[unit.start..unit.end]);
            if newest
                || !closed && recent.saturating_add(cost) <= self.config.preserve_recent_tokens
            {
                mandatory[index] = true;
                recent = recent.saturating_add(cost);
                newest = false;
            } else {
                closed = true;
            }
        }
        let removable = units
            .iter()
            .enumerate()
            .filter_map(|(index, unit)| (unit.removable && !mandatory[index]).then_some(index))
            .collect::<Vec<_>>();
        let mut removed = vec![false; units.len()];
        let mut position = 0;
        let mut remaining = before;
        let mut removed_count = 0usize;
        // An additive counter can choose its first candidate in one pass. Other counters
        // remove geometrically and recount whole valid units, never subtract BPE guesses.
        if measure.counting.additive_input() {
            while position < removable.len() {
                let index = removable[position];
                removed[index] = true;
                position += 1;
                remaining = remaining.saturating_sub(
                    measure.unit_cost(&input.messages[units[index].start..units[index].end]),
                );
                removed_count = removed_count.saturating_add(units[index].end - units[index].start);
                if remaining.saturating_add(measure.unit_cost(&[compaction_marker(removed_count)]))
                    <= target
                {
                    break;
                }
            }
        }
        loop {
            if position == 0 && position < removable.len() {
                removed[removable[position]] = true;
                position += 1;
            }
            let messages = rebuild_messages(input.messages, &units, &removed);
            let after = measure
                .count(&messages, input.tools)
                .tokens
                .min(usize::MAX as u64) as usize;
            if after <= target && after < before {
                return Ok(ContextCompaction {
                    messages,
                    estimated_tokens_before: before,
                    estimated_tokens_after: after,
                    removed_messages: removed_message_count(&units, &removed),
                    usage: ModelUsage::zero(),
                });
            }
            ensure!(
                position < removable.len(),
                "context cannot fit without removing system or recent messages"
            );
            let additional = (removable.len() - position).div_ceil(2).max(1);
            for index in &removable[position..position + additional] {
                removed[*index] = true;
            }
            position += additional;
        }
    }

    fn input_budget(&self, max_output_tokens: Option<u32>) -> usize {
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
    fn supports_counting(&self) -> bool {
        true
    }
    async fn compact_counted(
        &self,
        request: crate::summary::CompactionRequest<'_>,
        measure: ContextMeasure,
        _context: crate::TurnExecutionContext,
    ) -> Result<crate::summary::RecordedCompaction> {
        Ok(crate::summary::RecordedCompaction::transient(
            self.prune_counted(request.input, &measure)?,
        ))
    }

    fn uses_model(&self) -> bool {
        false
    }
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
                usage: ModelUsage::zero(),
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
        let mut remaining = estimated_tokens_before;
        let mut count = 0;
        for (index, unit) in units.iter().enumerate() {
            if !unit.removable || mandatory[index] {
                continue;
            }
            removed[index] = true;
            count += unit.end - unit.start;
            remaining =
                remaining.saturating_sub(estimate_messages(&input.messages[unit.start..unit.end]));
            let estimated_tokens_after =
                remaining.saturating_add(estimate_messages(&[compaction_marker(count)]));
            if estimated_tokens_after <= target_tokens {
                return Ok(ContextCompaction {
                    messages: rebuild_messages(input.messages, &units, &removed),
                    removed_messages: count,
                    estimated_tokens_before,
                    estimated_tokens_after,
                    usage: ModelUsage::zero(),
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
pub(crate) struct MessageUnit {
    pub(crate) start: usize,
    pub(crate) end: usize,
    removable: bool,
}

pub(crate) fn message_units(messages: &[Message]) -> Vec<MessageUnit> {
    let mut units = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let start = index;
        let removable = messages[index].role != MessageRole::System;
        index += 1;
        if messages[start].role == MessageRole::Assistant
            && messages[start].tool_calls().next().is_some()
        {
            while index < messages.len() && messages[index].role == MessageRole::Tool {
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

fn rebuild_messages(original: &[Message], units: &[MessageUnit], removed: &[bool]) -> Vec<Message> {
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

fn compaction_marker(removed_messages: usize) -> Message {
    Message::text(
        MessageRole::System,
        format!(
            "[Context compaction removed {removed_messages} older messages. Reinspect the workspace when earlier details are needed.]"
        ),
    )
}

/// Conservative model-independent estimate. Provider-specific compactors can replace it with an
/// exact tokenizer while preserving the same executor contract.
pub fn estimate_context_tokens(messages: &[Message], tools: Option<&[ToolDefinition]>) -> usize {
    crate::tokens::TokenCounting::default()
        .count(crate::tokens::TokenInput {
            provider: "",
            model: "",
            tokenizer: None,
            content: crate::tokens::TokenContent::Request {
                messages,
                tools,
                response_format: None,
                tool_choice: None,
            },
        })
        .tokens
        .min(usize::MAX as u64) as usize
}
fn estimate_messages(messages: &[Message]) -> usize {
    estimate_context_tokens(messages, None).saturating_sub(16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelToolCall;

    fn message(role: MessageRole, content: &str) -> Message {
        Message::text(role, content.to_string())
    }

    #[tokio::test]
    async fn pruning_preserves_system_recent_context_and_tool_units() {
        let mut assistant = message(MessageRole::Assistant, "calling tool");
        assistant
            .content
            .push(crate::model::ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({}),
                },
            });
        let messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, &"old request ".repeat(80)),
            assistant,
            message(MessageRole::Tool, &"old output ".repeat(80)),
            message(MessageRole::User, "latest request"),
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
        assert_eq!(compacted.messages[0].role, MessageRole::System);
        assert!(
            compacted
                .messages
                .iter()
                .any(|message| message.text_content().as_str() == "latest request")
        );
        assert!(
            !compacted
                .messages
                .iter()
                .any(|message| { message.tool_calls().next().is_some() })
        );
        assert!(
            !compacted
                .messages
                .iter()
                .any(|message| message.role == MessageRole::Tool)
        );
        assert!(compacted.estimated_tokens_after <= compactor.target_tokens(input));
    }

    #[tokio::test]
    async fn pruning_refuses_to_remove_oversized_recent_context() {
        let messages = vec![message(MessageRole::User, &"current request ".repeat(200))];
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
