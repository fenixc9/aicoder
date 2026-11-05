//! Read-only branch views and provider-safe projections, separate from durable messages.
use super::{Session, SessionEntry, SessionEntryData, SessionMessage};
use crate::model::{ContentBlock, Message, MessageRole};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContextSource {
    pub session_id: String,
    pub revision: u64,
    pub leaf_id: Option<String>,
    pub recorded_messages: usize,
    pub recovered_tool_results: usize,
    pub reordered_tool_results: usize,
    pub interrupted_turns: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub archived_messages: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_checkpoint: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextOrigin<'a> {
    Recorded {
        entry_id: &'a str,
    },
    RecoveredToolResult {
        call_entry_id: &'a str,
        call_id: &'a str,
    },
    InterruptedTurn {
        entry_id: Option<&'a str>,
        turn_id: &'a str,
    },
    Summary {
        entry_id: &'a str,
    },
    SummaryRecovery {
        entry_id: &'a str,
    },
}
/// Owned model messages with borrowed provenance; this view never changes the session.
#[derive(Debug)]
pub struct SessionContext<'a> {
    pub messages: Vec<Message>,
    pub origins: Vec<ContextOrigin<'a>>,
    pub source: SessionContextSource,
}
impl SessionContext<'_> {
    pub fn into_messages(self) -> Vec<Message> {
        self.messages
    }
}
/// Known durable usage on this branch. Interrupted unrecorded provider work remains unknown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchAccounting {
    pub usage: crate::model::ModelUsage,
    pub tokens: crate::run::TokenAccounting,
}
pub struct SessionBranch<'a> {
    session_id: &'a str,
    revision: u64,
    leaf_id: Option<&'a str>,
    entries: Vec<&'a SessionEntry>,
}
impl Session {
    /// Select a branch without checkout or storage I/O. None selects the empty root.
    pub fn branch(&self, leaf: Option<&str>) -> Result<SessionBranch<'_>> {
        let leaf = leaf
            .map(|id| {
                self.entry(id)
                    .ok_or_else(|| anyhow::anyhow!("context leaf does not exist"))
            })
            .transpose()?
            .map(|entry| entry.id.as_str());
        let mut entries = Vec::new();
        let mut cursor = leaf;
        while let Some(id) = cursor {
            let entry = self.entry(id).expect("validated session ancestry");
            entries.push(entry);
            cursor = entry.parent_id.as_deref();
        }
        entries.reverse();
        Ok(SessionBranch {
            session_id: &self.metadata().id,
            revision: self.metadata().revision,
            leaf_id: leaf,
            entries,
        })
    }
    pub fn selected_branch(&self) -> SessionBranch<'_> {
        self.branch(self.metadata().leaf_id.as_deref())
            .expect("validated selected leaf")
    }
}
impl<'a> SessionBranch<'a> {
    pub fn entries(&self) -> &[&'a SessionEntry] {
        &self.entries
    }
    pub fn leaf_id(&self) -> Option<&'a str> {
        self.leaf_id
    }
    pub fn messages(&self) -> impl Iterator<Item = SessionMessage<'a>> + '_ {
        self.entries.iter().filter_map(|entry| match &entry.data {
            data if data.as_message().is_some() => {
                let message = data.as_message().expect("message");
                Some(SessionMessage {
                    id: &entry.id,
                    parent_id: entry.parent_id.as_deref(),
                    created_at: entry.created_at,
                    message,
                })
            }
            _ => None,
        })
    }
    pub fn chat_messages(&self) -> Vec<Message> {
        self.messages().map(|entry| entry.message.clone()).collect()
    }
    pub fn selected_model(&self) -> Option<(&'a str, &'a str)> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.data {
                SessionEntryData::ModelChanged { provider, model } => {
                    Some((provider.as_str(), model.as_str()))
                }
                _ => None,
            })
    }
    /// Count summary work once: terminal turn statistics own automatic summary usage.
    /// Manual checkpoints and summaries from turns without terminal statistics are independent.
    pub fn accounting(&self) -> Result<BranchAccounting> {
        let mut total = BranchAccounting {
            usage: crate::model::ModelUsage::zero(),
            tokens: crate::run::TokenAccounting::default(),
        };
        let recorded_turns = self
            .entries
            .iter()
            .filter_map(|entry| match &entry.data {
                SessionEntryData::TurnEnd { turn_id, record } if record.statistics.is_some() => {
                    Some(turn_id.as_str())
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut add = |usage: &crate::model::ModelUsage, tokens: &crate::run::TokenAccounting| {
            total.usage.accumulate(usage);
            total.tokens.used = total.tokens.used.saturating_add(tokens.used);
            total.tokens.estimated |= tokens.estimated;
        };
        for entry in &self.entries {
            match &entry.data {
                SessionEntryData::TurnEnd { record, .. } => {
                    if let Some(statistics) = &record.statistics {
                        add(&statistics.usage, &statistics.tokens);
                    }
                }
                SessionEntryData::Extension { namespace, data }
                    if namespace == crate::summary::SUMMARY_NAMESPACE =>
                {
                    let checkpoint: crate::summary::SummaryCheckpoint =
                        serde_json::from_value(data.clone())?;
                    if checkpoint
                        .accounted_by_turn
                        .as_deref()
                        .is_none_or(|turn| !recorded_turns.contains(turn))
                    {
                        add(&checkpoint.draft.usage, &checkpoint.draft.tokens);
                    }
                }
                SessionEntryData::Extension { namespace, data }
                    if namespace == crate::summary::MANUAL_USAGE_NAMESPACE =>
                {
                    let record: crate::summary::manual::ManualSummaryUsage =
                        serde_json::from_value(data.clone())?;
                    record.validate()?;
                    add(&record.usage, &record.tokens);
                }
                _ => {}
            }
        }
        Ok(total)
    }
    pub fn model_context(&self) -> Result<SessionContext<'a>> {
        self.project(None)
    }
    /// A live turn may omit its own interruption marker only after all tool calls are paired.
    /// Its ID must be the open turn on this branch. Historical interruptions stay visible.
    pub fn context_for_active_turn(&self, turn_id: &str) -> Result<SessionContext<'a>> {
        self.project(Some(turn_id))
    }
    fn project(&self, active_turn: Option<&str>) -> Result<SessionContext<'a>> {
        let mut output = SessionContext {
            messages: Vec::new(),
            origins: Vec::new(),
            source: SessionContextSource {
                session_id: self.session_id.into(),
                revision: self.revision,
                leaf_id: self.leaf_id.map(str::to_owned),
                recorded_messages: 0,
                recovered_tool_results: 0,
                reordered_tool_results: 0,
                interrupted_turns: 0,
                archived_messages: 0,
                summary_checkpoint: None,
            },
        };
        let mut pending: Option<Pending<'a>> = None;
        let mut open = None;
        for entry in &self.entries {
            match &entry.data {
                SessionEntryData::TurnStart { turn_id } => {
                    flush(
                        &mut pending,
                        &mut output,
                        active_turn.is_some() && open == active_turn,
                    )?;
                    open = Some(turn_id.as_str());
                }
                SessionEntryData::Message { message }
                | SessionEntryData::NativeContext { message, .. } => {
                    output.source.recorded_messages += 1;
                    if message.role == MessageRole::Tool {
                        let tools = pending.as_mut().ok_or_else(|| {
                            anyhow::anyhow!("orphan tool result in session history")
                        })?;
                        let images = message
                            .content
                            .iter()
                            .filter(|block| matches!(block, ContentBlock::Image { .. }))
                            .count();
                        ensure!(
                            images <= 8,
                            "too many tool image references in session history"
                        );
                        if images > 0 {
                            ensure!(
                                message
                                    .content
                                    .iter()
                                    .filter(|block| matches!(
                                        block,
                                        ContentBlock::ToolResult { .. }
                                    ))
                                    .count()
                                    == 1,
                                "tool images require exactly one anchoring result"
                            );
                        }
                        for block in &message.content {
                            if matches!(block, ContentBlock::Image { .. }) {
                                continue;
                            }
                            let ContentBlock::ToolResult { call_id, .. } = block else {
                                bail!("tool message contains a non-result block")
                            };
                            ensure!(
                                tools.ids.contains(call_id.as_str()),
                                "tool result refers to an unknown call"
                            );
                            ensure!(
                                tools
                                    .results
                                    .insert(
                                        call_id.as_str(),
                                        (
                                            if images > 0 {
                                                message.content.iter().collect()
                                            } else {
                                                vec![block]
                                            },
                                            entry.id.as_str()
                                        )
                                    )
                                    .is_none(),
                                "duplicate tool result in session history"
                            );
                            tools.arrival.push(call_id);
                        }
                    } else {
                        flush(
                            &mut pending,
                            &mut output,
                            active_turn.is_some() && open == active_turn,
                        )?;
                        let calls = message
                            .tool_calls()
                            .map(|call| call.id.as_str())
                            .collect::<Vec<_>>();
                        if !calls.is_empty() {
                            ensure!(
                                message.role == MessageRole::Assistant,
                                "tool calls require an assistant message"
                            );
                            let ids = calls.iter().copied().collect::<HashSet<_>>();
                            ensure!(
                                ids.len() == calls.len(),
                                "duplicate tool call IDs in session history"
                            );
                            pending = Some(Pending {
                                order: calls,
                                ids,
                                results: HashMap::new(),
                                arrival: Vec::new(),
                                call_entry_id: &entry.id,
                            });
                        }
                        output.messages.push(message.clone());
                        output.origins.push(ContextOrigin::Recorded {
                            entry_id: &entry.id,
                        });
                    }
                }
                SessionEntryData::CustomMessage { custom } => {
                    flush(
                        &mut pending,
                        &mut output,
                        active_turn.is_some() && open == active_turn,
                    )?;
                    output.source.recorded_messages += 1;
                    output.messages.push(custom.message().clone());
                    output.origins.push(ContextOrigin::Recorded {
                        entry_id: &entry.id,
                    });
                }
                SessionEntryData::TurnEnd { turn_id, record } => {
                    flush(
                        &mut pending,
                        &mut output,
                        record.stop.is_none() || active_turn == Some(turn_id.as_str()),
                    )?;
                    if record.stop.is_some() {
                        interrupted(&mut output, Some(&entry.id), turn_id);
                    }
                    if open == Some(turn_id.as_str()) {
                        open = None;
                    }
                }
                _ => {}
            }
        }
        ensure!(
            active_turn.is_none() || active_turn == open,
            "active context turn does not match the selected branch"
        );
        flush(&mut pending, &mut output, active_turn.is_some())?;
        if let Some(turn_id) = open
            && active_turn != Some(turn_id)
        {
            interrupted(&mut output, None, turn_id);
        }
        if let Some((position, entry, record)) =
            self.entries
                .iter()
                .enumerate()
                .rev()
                .find_map(|(position, entry)| match &entry.data {
                    SessionEntryData::Extension { namespace, data }
                        if namespace == crate::summary::SUMMARY_NAMESPACE =>
                    {
                        Some((
                            position,
                            *entry,
                            serde_json::from_value::<crate::summary::SummaryCheckpoint>(
                                data.clone(),
                            ),
                        ))
                    }
                    _ => None,
                })
        {
            let record = record?;
            crate::summary::validate_checkpoint(
                &record,
                &self.entries[..position],
                entry.parent_id.as_deref(),
            )?;
            let offset=output.origins.iter().position(|origin|matches!(origin,ContextOrigin::Recorded{entry_id} if *entry_id==record.first_kept_entry_id)).ok_or_else(||anyhow::anyhow!("summary tail is not a projected message boundary"))?;
            let mut uncertain = record.draft.uncertain_operations
                || crate::summary::uncertain(&output.messages[..offset]);
            let mut messages = Vec::new();
            let mut origins = Vec::new();
            for (message, origin) in output
                .messages
                .drain(offset..)
                .zip(output.origins.drain(offset..))
            {
                let old_marker = matches!(&origin,ContextOrigin::InterruptedTurn{entry_id:Some(id),..} if self.entries[..position].iter().any(|entry|entry.id==*id));
                if old_marker {
                    uncertain = true;
                } else {
                    messages.push(message);
                    origins.push(origin);
                }
            }
            output.messages = Vec::new();
            output.origins = Vec::new();
            if uncertain {
                output.messages.push(crate::summary::recovery_notice());
                output.origins.push(ContextOrigin::SummaryRecovery {
                    entry_id: &entry.id,
                });
            }
            output.messages.push(record.draft.summary.message());
            output.origins.push(ContextOrigin::Summary {
                entry_id: &entry.id,
            });
            output.messages.extend(messages);
            output.origins.extend(origins);
            output.source.summary_checkpoint = Some(entry.id.clone());
        }
        Ok(output)
    }
}
struct Pending<'a> {
    order: Vec<&'a str>,
    ids: HashSet<&'a str>,
    results: HashMap<&'a str, (Vec<&'a ContentBlock>, &'a str)>,
    arrival: Vec<&'a str>,
    call_entry_id: &'a str,
}
fn flush<'a>(
    pending: &mut Option<Pending<'a>>,
    output: &mut SessionContext<'a>,
    strict: bool,
) -> Result<()> {
    if let Some(mut tools) = pending.take() {
        ensure!(
            !strict || tools.order.len() == tools.results.len(),
            "completed or active turn has unfinished tool calls"
        );
        let recorded = tools
            .order
            .iter()
            .copied()
            .filter(|id| tools.results.contains_key(id))
            .collect::<Vec<_>>();
        output.source.reordered_tool_results += recorded
            .iter()
            .zip(&tools.arrival)
            .filter(|(a, b)| a != b)
            .count();
        for id in tools.order {
            let (content, origin) = match tools.results.remove(id) {
                Some((blocks, entry_id)) => (
                    blocks.into_iter().cloned().collect(),
                    ContextOrigin::Recorded { entry_id },
                ),
                None => {
                    output.source.recovered_tool_results += 1;
                    (
                        vec![ContentBlock::ToolResult {
                            call_id: id.into(),
                            content: serde_json::json!({"ok":false,"error":{"code":"execution_interrupted","message":"No completed result was recorded. The operation may have run partially; inspect the workspace before retrying."}}),
                            is_error: true,
                        }],
                        ContextOrigin::RecoveredToolResult {
                            call_entry_id: tools.call_entry_id,
                            call_id: id,
                        },
                    )
                }
            };
            output.messages.push(Message {
                role: MessageRole::Tool,
                content,
            });
            output.origins.push(origin);
        }
    }
    Ok(())
}
fn interrupted<'a>(output: &mut SessionContext<'a>, entry_id: Option<&'a str>, turn_id: &'a str) {
    output.messages.push(Message::text(MessageRole::System,"[Previous execution was interrupted. Recorded results remain authoritative; unrecorded operations may have run partially. Inspect the workspace before retrying. Partial response fragments are not a completed answer.]"));
    output
        .origins
        .push(ContextOrigin::InterruptedTurn { entry_id, turn_id });
    output.source.interrupted_turns += 1;
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}
