use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use aicoder_core::{
    AgentRawEvent, AgentRawEventEnvelope, AgentRunState, AgentTurnOutcome, ApprovalRequestId,
    RunHandle, RunInputKind, SessionSelection, ToolExecutionOutcome, TurnExecutionContext,
    model::{Message, MessageRole, ModelUsage},
    session::{Session, SessionEntryData, SessionInfo},
    tools::ToolInvocation,
};
use unicode_width::UnicodeWidthStr;

use crate::commands::{self, CommandSpec};

pub enum AppEvent {
    ModelSelected(Box<Result<aicoder_core::session::ModelSelectionReport, String>>),
    Agent(AgentRawEventEnvelope),
    TurnFinished(Box<AgentTurnOutcome>),
    CompactionFinished(Box<aicoder_core::summary::ManualCompactionReport>),
    ObservationFailed(String),
    RunDropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sessions,
    Input,
}

#[derive(Debug)]
pub enum TimelineItem {
    User(String),
    Assistant {
        text: String,
        open: bool,
    },
    Reasoning {
        text: String,
        open: bool,
    },
    Tool {
        name: String,
        arguments: String,
        status: String,
        output: Option<String>,
    },
    Notice(String),
    Error(String),
}

pub struct PendingApproval {
    pub invocation: ToolInvocation,
    pub id: ApprovalRequestId,
    pub scroll: u16,
    pub show_arguments: bool,
    pub session: Option<aicoder_core::tools::ApprovalSessionInfo>,
}

#[derive(Default)]
pub struct InputBuffer {
    value: String,
    cursor: usize,
}

impl InputBuffer {
    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn cursor(&self) -> usize {
        UnicodeWidthStr::width(&self.value[..self.cursor])
    }

    pub fn insert(&mut self, character: char) {
        self.value.insert(self.cursor, character);
        self.cursor += character.len_utf8();
    }

    pub fn replace(&mut self, value: String) {
        self.cursor = value.len();
        self.value = value;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let previous = self.value[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.value.drain(previous..self.cursor);
        self.cursor = previous;
    }

    pub fn delete(&mut self) {
        if self.cursor == self.value.len() {
            return;
        }
        let next = self.value[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(index, _)| self.cursor + index)
            .unwrap_or(self.value.len());
        self.value.drain(self.cursor..next);
    }

    pub fn move_left(&mut self) {
        self.cursor = self.value[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0);
    }

    pub fn move_right(&mut self) {
        self.cursor = self.value[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(index, _)| self.cursor + index)
            .unwrap_or(self.value.len());
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.value.len();
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.value)
    }
}

pub struct App {
    pub run: Option<crate::runtime::ActiveRun>,
    observation_incomplete: bool,
    pub sessions: Vec<SessionInfo>,
    pub selected_session: usize,
    pub session_selection: SessionSelection,
    pub timeline: Vec<TimelineItem>,
    pub input: InputBuffer,
    pub focus: Focus,
    pub state: AgentRunState,
    pub usage: ModelUsage,
    pub model_label: Option<String>,
    pub round: usize,
    pub active_context: Option<TurnExecutionContext>,
    pub active_handle: Option<RunHandle>,
    pub started_at: Option<Instant>,
    pub elapsed: Duration,
    pub approval: Option<PendingApproval>,
    pub confirm_delete: bool,
    pub should_quit: bool,
    pub scroll_back: u16,
    slash_selection: usize,
    slash_menu_dismissed: bool,
    tool_indexes: HashMap<String, usize>,
}

impl App {
    pub fn new(sessions: Vec<SessionInfo>) -> Self {
        Self {
            run: None,
            observation_incomplete: false,
            sessions,
            selected_session: 0,
            session_selection: SessionSelection::New,
            timeline: Vec::new(),
            input: InputBuffer::default(),
            focus: Focus::Input,
            state: AgentRunState::Idle,
            usage: ModelUsage::zero(),
            model_label: None,
            round: 0,
            active_context: None,
            active_handle: None,
            started_at: None,
            elapsed: Duration::ZERO,
            approval: None,
            confirm_delete: false,
            should_quit: false,
            scroll_back: 0,
            slash_selection: 0,
            slash_menu_dismissed: false,
            tool_indexes: HashMap::new(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.active_context.is_some()
    }

    pub fn slash_suggestions(&self) -> Vec<&'static CommandSpec> {
        if self.slash_menu_dismissed || self.focus != Focus::Input || self.is_running() {
            return Vec::new();
        }
        commands::suggestions(self.input.value())
    }

    pub fn selected_slash_command(&self) -> Option<&'static CommandSpec> {
        let suggestions = self.slash_suggestions();
        suggestions
            .get(
                self.slash_selection
                    .min(suggestions.len().saturating_sub(1)),
            )
            .copied()
    }

    pub fn slash_selection(&self) -> usize {
        self.slash_selection
    }

    pub fn complete_selected_slash_command(&mut self) -> bool {
        let Some(spec) = self.selected_slash_command() else {
            return false;
        };
        self.input.replace(commands::completion(spec));
        self.slash_selection = 0;
        self.slash_menu_dismissed = true;
        true
    }

    pub fn select_next_slash_command(&mut self) {
        let count = self.slash_suggestions().len();
        if count > 0 {
            self.slash_selection = (self.slash_selection + 1) % count;
        }
    }

    pub fn select_previous_slash_command(&mut self) {
        let count = self.slash_suggestions().len();
        if count > 0 {
            self.slash_selection = (self.slash_selection + count - 1) % count;
        }
    }

    pub fn slash_input_changed(&mut self) {
        self.slash_selection = 0;
        self.slash_menu_dismissed = false;
    }

    pub fn dismiss_slash_menu(&mut self) {
        self.slash_menu_dismissed = true;
    }

    pub fn begin_model_selection(&mut self, context: TurnExecutionContext) {
        self.active_context = Some(context);
        self.active_handle = None;
        self.started_at = Some(Instant::now());
        self.state = AgentRunState::Preparing;
        self.timeline
            .push(TimelineItem::Notice("Checking selected model...".into()));
    }
    pub fn begin_compaction(&mut self, context: TurnExecutionContext) {
        self.active_context = Some(context);
        self.active_handle = None;
        self.started_at = Some(Instant::now());
        self.state = AgentRunState::Compacting { round: 0 };
        self.timeline.push(TimelineItem::Notice(
            "Summarizing session context...".into(),
        ));
    }
    pub fn begin_turn(&mut self, prompt: String, context: TurnExecutionContext) {
        self.observation_incomplete = false;
        self.timeline.push(TimelineItem::User(prompt));
        self.active_context = Some(context);
        self.started_at = Some(Instant::now());
        self.elapsed = Duration::ZERO;
        self.state = AgentRunState::Preparing;
        self.usage = ModelUsage::zero();
        self.round = 0;
        self.scroll_back = 0;
    }

    pub fn cancel(&mut self, reason: &str) {
        if let Some(context) = &self.active_context {
            if let Some(handle) = &self.active_handle {
                let _ = handle.cancel(reason);
            } else {
                context.cancel(reason);
            }
            self.timeline
                .push(TimelineItem::Notice("Cancelling active turn...".into()));
        }
        self.approval = None;
    }

    pub fn queue_input(&mut self, kind: RunInputKind, prompt: String) -> bool {
        let Some(handle) = &self.active_handle else {
            self.input.replace(prompt);
            return false;
        };
        let message = Message::text(MessageRole::User, prompt.clone());
        let result = match kind {
            RunInputKind::Steer => handle.steer(message),
            RunInputKind::FollowUp => handle.follow_up(message),
        };
        let accepted = match result {
            Ok(id) => {
                self.timeline.push(TimelineItem::Notice(format!(
                    "Queued {kind:?} #{id}: {prompt}"
                )));
                true
            }
            Err(error) => {
                self.input.replace(prompt);
                self.timeline.push(TimelineItem::Error(error.to_string()));
                false
            }
        };
        self.scroll_back = 0;
        accepted
    }
    pub fn resolve_approval(&mut self, approved: bool) {
        self.resolve_approval_decision(approved.into());
    }
    pub fn resolve_approval_decision(&mut self, decision: aicoder_core::tools::ApprovalDecision) {
        if let Some(approval) = &self.approval
            && let Some(handle) = &self.active_handle
        {
            match handle.respond_approval_decision(&approval.id, decision) {
                Ok(()) => self.approval = None,
                Err(error) => self.timeline.push(TimelineItem::Notice(error.to_string())),
            }
        }
    }

    pub fn apply(&mut self, event: AppEvent) {
        match event {
            AppEvent::Agent(envelope) => self.apply_agent_event(envelope),
            AppEvent::TurnFinished(outcome) => self.finish_turn(*outcome),
            AppEvent::ModelSelected(report) => {
                self.run = None;
                self.active_context = None;
                self.active_handle = None;
                self.started_at = None;
                match *report {
                    Ok(report) => {
                        self.model_label =
                            Some(format!("{}/{}", report.spec.provider, report.spec.id));
                        self.state = AgentRunState::Completed { rounds: 0 };
                        self.timeline.push(TimelineItem::Notice(format!(
                            "Model selected: {}; estimated input {} tokens",
                            self.model_label.as_deref().unwrap_or_default(),
                            report.input_token_estimate
                        )));
                    }
                    Err(error) => {
                        self.state = AgentRunState::Failed { round: None };
                        self.timeline.push(TimelineItem::Error(error));
                    }
                }
            }
            AppEvent::CompactionFinished(report) => {
                self.run = None;
                self.active_context = None;
                self.active_handle = None;
                self.started_at = None;
                if let Some(usage) = &report.branch_usage {
                    self.usage = usage.clone();
                } else {
                    self.usage.accumulate(&report.usage);
                }
                self.state = if report.succeeded() {
                    AgentRunState::Completed { rounds: 0 }
                } else {
                    AgentRunState::Failed { round: None }
                };
                self.timeline.push(TimelineItem::Notice(format!(
                    "Manual compaction: {} -> {} estimated tokens; {} attempts; {}",
                    report.before,
                    report.after,
                    report.attempts,
                    if report.succeeded() {
                        "complete".into()
                    } else {
                        format!("{:?} {:?}", report.stop, report.persistence_error)
                    }
                )));
            }
            AppEvent::ObservationFailed(error) => {
                self.observation_incomplete = true;
                self.timeline.push(TimelineItem::Notice(format!("Live observation ended: {error}. The run continues; its result will restore the transcript.")));
            }
            AppEvent::RunDropped => {
                if let Some(handle) = &self.active_handle
                    && let Some(id) = handle.snapshot().session_id
                {
                    self.session_selection = SessionSelection::Existing(id);
                }
                self.timeline.push(TimelineItem::Error("Run task ended without a result. Reopen the session to inspect recorded progress.".into()));
                self.state = AgentRunState::Failed {
                    round: (self.round > 0).then_some(self.round),
                };
                self.active_context = None;
                self.active_handle = None;
                self.approval = None;
                self.run = None;
            }
        }
    }

    fn apply_agent_event(&mut self, envelope: AgentRawEventEnvelope) {
        self.round = self.round.max(envelope.round.unwrap_or(0));
        match envelope.event {
            AgentRawEvent::AgentStarted { model } => self.model_label = Some(model.to_string()),
            AgentRawEvent::RunInputApplied { input } => {
                self.timeline.push(TimelineItem::Notice(format!(
                    "Applied {:?} #{}",
                    input.kind, input.id
                )));
                push_message(&mut self.timeline, &input.message);
            }
            AgentRawEvent::RunApprovalRequested { request } => {
                if self.active_handle.as_ref().is_some_and(|handle| {
                    handle
                        .snapshot()
                        .approvals
                        .iter()
                        .any(|pending| pending.id == request.id)
                }) {
                    self.approval = Some(PendingApproval {
                        id: request.id.clone(),
                        scroll: 0,
                        show_arguments: false,
                        session: request.session.clone(),
                        invocation: request.invocation.clone(),
                    });
                }
            }
            AgentRawEvent::ToolApprovalResolved { .. } => {
                if let Some(pending) = &self.approval
                    && self.active_handle.as_ref().is_none_or(|handle| {
                        !handle
                            .snapshot()
                            .approvals
                            .iter()
                            .any(|request| request.id == pending.id)
                    })
                {
                    self.approval = None;
                }
            }
            AgentRawEvent::StateChanged { transition } => self.state = transition.current,
            AgentRawEvent::ReasoningStarted { .. } => self.timeline.push(TimelineItem::Reasoning {
                text: String::new(),
                open: true,
            }),
            AgentRawEvent::ReasoningChunk { delta, .. } => {
                append_stream(&mut self.timeline, true, &delta)
            }
            AgentRawEvent::ReasoningEnded { .. } => close_stream(&mut self.timeline, true),
            AgentRawEvent::ContentStarted { .. } => self.timeline.push(TimelineItem::Assistant {
                text: String::new(),
                open: true,
            }),
            AgentRawEvent::ContentChunk { delta, .. } => {
                append_stream(&mut self.timeline, false, &delta)
            }
            AgentRawEvent::ContentEnded { .. } => close_stream(&mut self.timeline, false),
            AgentRawEvent::ToolExecutionStarted {
                call_id,
                name,
                arguments,
            } => {
                let index = self.timeline.len();
                self.tool_indexes.insert(call_id.to_string(), index);
                self.timeline.push(TimelineItem::Tool {
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                    status: "running".into(),
                    output: None,
                });
            }
            AgentRawEvent::ToolExecutionEnded {
                call_id,
                outcome,
                artifacts,
                ..
            } => {
                for artifact in artifacts {
                    self.timeline.push(TimelineItem::Notice(format!(
                        "Saved output: {} ({} bytes, complete={}); use read_artifact to inspect",
                        artifact.id, artifact.bytes, artifact.complete
                    )));
                }
                if let Some(index) = self.tool_indexes.get(call_id.as_ref()).copied()
                    && let Some(TimelineItem::Tool { status, output, .. }) =
                        self.timeline.get_mut(index)
                {
                    *status = tool_status(outcome.as_ref());
                    *output = tool_output(outcome.as_ref());
                }
            }
            AgentRawEvent::ModelRetryScheduled { attempt, delay, .. } => {
                self.timeline.push(TimelineItem::Notice(format!(
                    "Model retry {attempt} in {:.1}s",
                    delay.as_secs_f32()
                )))
            }
            AgentRawEvent::ContextCompactionCompleted {
                removed_messages, ..
            } => self.timeline.push(TimelineItem::Notice(format!(
                "Context compacted, removed {removed_messages} messages"
            ))),
            AgentRawEvent::UsageUpdated { usage } => self.usage.accumulate(&usage),
            AgentRawEvent::AgentCompleted { usage, .. } => self.usage = (*usage).clone(),
            AgentRawEvent::AgentFailed { message, .. } => {
                self.timeline.push(TimelineItem::Error(message.to_string()));
                self.approval = None;
            }
            AgentRawEvent::AgentAborted { reason } => {
                self.timeline
                    .push(TimelineItem::Notice(format!("Turn cancelled: {reason}")));
                self.approval = None;
            }
            _ => {}
        }
        self.scroll_back = 0;
    }

    fn finish_turn(&mut self, outcome: AgentTurnOutcome) {
        if self.observation_incomplete {
            self.timeline.clear();
            self.tool_indexes.clear();
            for message in &outcome.progress().messages {
                push_message(&mut self.timeline, message);
            }
            if let Some(partial) = &outcome.progress().partial_response {
                if !partial.reasoning.is_empty() {
                    self.timeline.push(TimelineItem::Reasoning {
                        text: partial.reasoning.clone(),
                        open: false,
                    });
                }
                if !partial.text.is_empty() {
                    self.timeline.push(TimelineItem::Assistant {
                        text: format!("[Partial response] {}", partial.text),
                        open: false,
                    });
                }
            }
            self.timeline.push(TimelineItem::Notice(
                "Transcript restored from the authoritative run result.".into(),
            ));
            self.observation_incomplete = false;
        }
        self.run = None;
        match outcome {
            AgentTurnOutcome::Completed(result) => {
                self.round = result.execution_result.rounds;
                self.state = AgentRunState::Completed {
                    rounds: result.execution_result.rounds,
                };
                if let Some(session) = result.session {
                    self.session_selection = SessionSelection::Existing(session.id);
                }
                self.usage = result.execution_result.progress.usage;
            }
            AgentTurnOutcome::Aborted(turn)
            | AgentTurnOutcome::Limited(turn)
            | AgentTurnOutcome::Stopped(turn)
            | AgentTurnOutcome::Failed(turn) => {
                for input in &turn.progress.pending_inputs {
                    self.timeline.push(TimelineItem::Notice(format!(
                        "Not applied: {:?} #{}: {}",
                        input.kind,
                        input.id,
                        input.message.text_content()
                    )));
                }
                self.round = turn.progress.rounds;
                let round = (self.round > 0).then_some(self.round);
                self.state = match &turn.stop {
                    aicoder_core::RunStop::Cancelled { .. } => AgentRunState::Aborted { round },
                    aicoder_core::RunStop::Failed(_) => AgentRunState::Failed { round },
                    _ => AgentRunState::Stopped { round },
                };
                let message = turn.stop.to_string();
                if let Some(failure) = turn.persistence_error {
                    self.timeline.push(TimelineItem::Error(format!(
                        "Session could not be saved: {failure}"
                    )));
                }
                self.usage = turn.progress.usage;
                if let Some(session) = turn.session {
                    self.session_selection = SessionSelection::Existing(session.id);
                }
                if !self.timeline.iter().rev().take(2).any(|item|matches!(item,TimelineItem::Error(text)|TimelineItem::Notice(text) if text.contains(&message))) {
                    self.timeline.push(if matches!(turn.stop,aicoder_core::RunStop::Failed(_)) {TimelineItem::Error(message)} else {TimelineItem::Notice(format!("Turn stopped: {message}"))});
                }
            }
        }
        self.elapsed = self
            .started_at
            .map(|started| started.elapsed())
            .unwrap_or(self.elapsed);
        self.active_context = None;
        self.active_handle = None;
        self.started_at = None;
        self.approval = None;
    }

    pub fn load_session(&mut self, session: &Session) {
        self.model_label = session
            .selected_model()
            .map(|(provider, model)| format!("{provider}/{model}"));
        self.timeline.clear();
        self.tool_indexes.clear();
        self.usage = session
            .selected_branch()
            .accounting()
            .map(|accounting| accounting.usage)
            .unwrap_or_else(|_| ModelUsage::zero());
        for entry in session.active_entries() {
            match &entry.data {
                SessionEntryData::Message { message } => push_message(&mut self.timeline, message),
                SessionEntryData::TurnEnd { record, .. } => {
                    for input in &record.pending_inputs {
                        self.timeline.push(TimelineItem::Notice(format!(
                            "Not applied: {:?} #{}: {}",
                            input.kind,
                            input.id,
                            input.message.text_content()
                        )));
                    }
                    if let Some(stop) = &record.stop {
                        self.timeline
                            .push(TimelineItem::Notice(format!("Interrupted: {stop}")));
                    }
                    if let Some(partial) = &record.partial_response {
                        if !partial.reasoning.is_empty() {
                            self.timeline.push(TimelineItem::Reasoning {
                                text: partial.reasoning.clone(),
                                open: false,
                            });
                        }
                        if !partial.text.is_empty() {
                            self.timeline.push(TimelineItem::Assistant {
                                text: format!("[Partial response] {}", partial.text),
                                open: false,
                            });
                        }
                        if !partial.tool_calls.is_empty() {
                            self.timeline.push(TimelineItem::Notice(format!(
                                "{} incomplete tool call fragments; execution was not resumed",
                                partial.tool_calls.len()
                            )));
                        }
                    }
                }
                _ => {}
            }
        }
        if session.unfinished_turn().is_some() {
            self.timeline.push(TimelineItem::Notice("Previous process ended without a terminal record. Inspect the workspace before retrying operations.".into()));
        }
        self.session_selection = SessionSelection::Existing(session.metadata().id.clone());
        self.focus = Focus::Input;
        self.scroll_back = 0;
    }

    pub fn select_new_session(&mut self) {
        self.session_selection = SessionSelection::New;
        self.model_label = None;
        self.timeline.clear();
        self.tool_indexes.clear();
        self.focus = Focus::Input;
    }
}

fn append_stream(timeline: &mut Vec<TimelineItem>, reasoning: bool, delta: &str) {
    let target = timeline.iter_mut().rev().find(|item| match item {
        TimelineItem::Reasoning { open, .. } => reasoning && *open,
        TimelineItem::Assistant { open, .. } => !reasoning && *open,
        _ => false,
    });
    match target {
        Some(TimelineItem::Reasoning { text, .. }) | Some(TimelineItem::Assistant { text, .. }) => {
            text.push_str(delta)
        }
        _ if reasoning => timeline.push(TimelineItem::Reasoning {
            text: delta.into(),
            open: true,
        }),
        _ => timeline.push(TimelineItem::Assistant {
            text: delta.into(),
            open: true,
        }),
    }
}

fn close_stream(timeline: &mut [TimelineItem], reasoning: bool) {
    if let Some(TimelineItem::Reasoning { open, .. } | TimelineItem::Assistant { open, .. }) =
        timeline.iter_mut().rev().find(|item| match item {
            TimelineItem::Reasoning { open, .. } => reasoning && *open,
            TimelineItem::Assistant { open, .. } => !reasoning && *open,
            _ => false,
        })
    {
        *open = false;
    }
}

fn tool_status(outcome: &ToolExecutionOutcome) -> String {
    match outcome {
        ToolExecutionOutcome::Succeeded { truncated, .. } => if *truncated {
            "done (truncated)"
        } else {
            "done"
        }
        .into(),
        ToolExecutionOutcome::Failed { code, .. } => format!("failed: {code}"),
        ToolExecutionOutcome::TimedOut => "timed out".into(),
        ToolExecutionOutcome::ApprovalDenied => "denied".into(),
        ToolExecutionOutcome::Cancelled => "cancelled".into(),
    }
}

fn tool_output(outcome: &ToolExecutionOutcome) -> Option<String> {
    match outcome {
        ToolExecutionOutcome::Succeeded { output, .. } => {
            Some(serde_json::to_string_pretty(output).unwrap_or_else(|_| output.to_string()))
        }
        ToolExecutionOutcome::Failed { message, .. } => Some(message.clone()),
        ToolExecutionOutcome::TimedOut => Some("Tool execution exceeded its timeout".into()),
        ToolExecutionOutcome::ApprovalDenied => Some("User denied tool execution".into()),
        ToolExecutionOutcome::Cancelled => Some("Tool execution was cancelled".into()),
    }
}

fn push_message(timeline: &mut Vec<TimelineItem>, message: &Message) {
    for block in &message.content {
        match block {
            aicoder_core::model::ContentBlock::Reasoning { text } => {
                timeline.push(TimelineItem::Reasoning {
                    text: text.clone(),
                    open: false,
                })
            }
            aicoder_core::model::ContentBlock::Text { text }
                if message.role == MessageRole::User =>
            {
                timeline.push(TimelineItem::User(text.clone()))
            }
            aicoder_core::model::ContentBlock::Text { text }
                if message.role == MessageRole::Assistant =>
            {
                timeline.push(TimelineItem::Assistant {
                    text: text.clone(),
                    open: false,
                })
            }
            aicoder_core::model::ContentBlock::ToolCall { call } => {
                timeline.push(TimelineItem::Tool {
                    name: call.name.clone(),
                    arguments: call.arguments.to_string(),
                    status: "recorded".into(),
                    output: None,
                })
            }
            aicoder_core::model::ContentBlock::ToolResult { content, .. } => {
                timeline.push(TimelineItem::Tool {
                    name: "tool".into(),
                    arguments: String::new(),
                    status: "recorded".into(),
                    output: Some(content.to_string()),
                })
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aicoder_core::{
        AgentRawEvent, AgentRawEventEnvelope, RunId, ToolExecutionOutcome, events::StreamEnd,
    };
    use serde_json::json;

    use super::*;

    fn event(sequence: u64, event: AgentRawEvent) -> AppEvent {
        AppEvent::Agent(AgentRawEventEnvelope {
            schema_version: aicoder_core::events::EVENT_SCHEMA_VERSION,
            session_id: None,
            turn_id: None,
            elapsed_ms: 0,
            termination: None,
            run_id: RunId::new(),
            sequence,
            round: Some(1),
            event,
        })
    }

    #[test]
    fn saved_output_reference_is_visible_even_when_the_tool_payload_is_only_a_preview() {
        let mut app = App::new(vec![]);
        let reference = aicoder_core::artifacts::ArtifactRef {
            version: 1,
            id: "00000000-0000-0000-0000-000000000001".into(),
            media_type: "text/plain".into(),
            bytes: 200000,
            complete: true,
        };
        app.apply(event(
            1,
            AgentRawEvent::ToolExecutionStarted {
                call_id: "call".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            },
        ));
        app.apply(event(
            2,
            AgentRawEvent::ToolExecutionEnded {
                artifacts: vec![reference.clone()],
                call_id: "call".into(),
                name: "bash".into(),
                outcome: ToolExecutionOutcome::Succeeded {
                    output: json!({"stdout":"preview"}),
                    truncated: true,
                }
                .into(),
            },
        ));
        assert!(app.timeline.iter().any(|item|matches!(item,TimelineItem::Notice(text) if text.contains(&reference.id) && text.contains("200000"))));
    }

    #[test]
    fn model_selection_updates_the_header_without_inventing_a_chat_turn() {
        let mut app = App::new(vec![]);
        app.model_label = Some("previous".into());
        app.timeline.push(TimelineItem::User("existing".into()));
        app.begin_model_selection(TurnExecutionContext::new());
        app.apply(AppEvent::ModelSelected(Box::new(Err("unsupported".into()))));
        assert_eq!(app.model_label.as_deref(), Some("previous"));
        let report = aicoder_core::session::ModelSelectionReport {
            spec: aicoder_core::model::ModelSpec::new("host", "selected"),
            input_token_estimate: 30,
            output_reservation: 10,
            context_limit: Some(1000),
            needs_compaction: false,
            omitted_reasoning_blocks: 0,
        };
        app.begin_model_selection(TurnExecutionContext::new());
        app.apply(AppEvent::ModelSelected(Box::new(Ok(report))));
        assert_eq!(app.model_label.as_deref(), Some("host/selected"));
        assert!(!app.is_running());
        assert_eq!(
            app.timeline
                .iter()
                .filter(|item| matches!(item, TimelineItem::User(_)))
                .count(),
            1
        );
        app.select_new_session();
        assert!(app.model_label.is_none());
    }

    #[test]
    fn manual_compaction_keeps_transcript_and_restores_durable_usage() {
        let mut app = App::new(vec![]);
        app.timeline
            .push(TimelineItem::User("existing user message".into()));
        app.begin_compaction(TurnExecutionContext::new());
        assert!(app.is_running());
        let usage = ModelUsage {
            total_tokens: Some(12),
            ..ModelUsage::zero()
        };
        let report = aicoder_core::summary::ManualCompactionReport {
            session: None,
            checkpoint_entry_id: Some("checkpoint".into()),
            usage: usage.clone(),
            branch_usage: Some(usage),
            tokens: Default::default(),
            attempts: 1,
            before: 200,
            after: 80,
            stop: None,
            persistence_error: None,
        };
        app.apply(AppEvent::CompactionFinished(Box::new(report)));
        assert!(!app.is_running());
        assert_eq!(app.usage.total_tokens, Some(12));
        assert_eq!(
            app.timeline
                .iter()
                .filter(|item| matches!(item, TimelineItem::User(_)))
                .count(),
            1
        );
        assert!(app.active_context.is_none());
    }

    #[test]
    fn incomplete_live_timeline_is_rebuilt_from_the_run_result_without_erasing_the_draft() {
        use aicoder_core::{AgentTurnResult, RunProgress, TurnExecutionResult};
        let mut app = App::new(vec![]);
        app.begin_turn("initial".into(), TurnExecutionContext::new());
        app.input.replace("next draft".into());
        app.timeline.push(TimelineItem::Assistant {
            text: "incomplete".into(),
            open: true,
        });
        app.apply(AppEvent::ObservationFailed("lagged".into()));
        let mut progress = RunProgress::new(vec![
            Message::text(MessageRole::User, "initial"),
            Message::text(MessageRole::Assistant, "complete answer"),
        ]);
        progress.rounds = 1;
        app.apply(AppEvent::TurnFinished(Box::new(
            AgentTurnOutcome::Completed(AgentTurnResult {
                execution_result: TurnExecutionResult {
                    final_message: Message::text(MessageRole::Assistant, "complete answer"),
                    finish_reason: Some(aicoder_core::model::StopReason::Complete),
                    progress,
                },
                session: None,
            }),
        )));
        assert_eq!(app.input.value(), "next draft");
        assert!(!app.is_running());
        assert!(app.timeline.iter().any(|item|matches!(item,TimelineItem::Assistant {text,open:false} if text=="complete answer")));
        assert!(
            !app.timeline
                .iter()
                .any(|item| matches!(item,TimelineItem::Assistant {text,..} if text=="incomplete"))
        );
    }

    #[test]
    fn queued_controls_wait_for_application_and_rejected_text_is_restored() {
        use aicoder_core::{RunControlConfig, RunHandle};
        let context = TurnExecutionContext::new();
        let (handle, control) = RunHandle::channel(
            context.clone(),
            RunControlConfig {
                max_pending_inputs: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let mut app = App::new(vec![]);
        app.begin_turn("initial".into(), context);
        app.active_handle = Some(handle.clone());
        app.queue_input(RunInputKind::Steer, "redirect".into());
        assert!(
            matches!(app.timeline.last(),Some(TimelineItem::Notice(text)) if text.contains("Queued"))
        );
        assert_eq!(
            app.timeline
                .iter()
                .filter(|item| matches!(item, TimelineItem::User(_)))
                .count(),
            1
        );
        app.queue_input(RunInputKind::FollowUp, "keep this draft".into());
        assert_eq!(app.input.value(), "keep this draft");
        let pending = handle.snapshot().pending_inputs[0].clone();
        app.apply(event(
            1,
            AgentRawEvent::RunInputApplied {
                input: Arc::new(pending),
            },
        ));
        assert!(matches!(app.timeline.last(),Some(TimelineItem::User(text)) if text=="redirect"));
        drop(control);
    }

    #[test]
    fn session_reload_shows_only_the_selected_branch_and_labels_partial_output() {
        use aicoder_core::{
            run::PartialModelResponse,
            session::{MemorySessionRepository, SessionRepository, TurnRecord},
        };
        let workspace = tempfile::tempdir().unwrap();
        let repository = MemorySessionRepository::new();
        let mut session = repository.create(workspace.path()).unwrap();
        repository
            .append(&mut session, Message::text(MessageRole::User, "root"))
            .unwrap();
        let root = session.metadata().leaf_id.clone().unwrap();
        repository
            .append(
                &mut session,
                Message::text(MessageRole::Assistant, "hidden branch"),
            )
            .unwrap();
        repository.checkout(&mut session, Some(&root)).unwrap();
        let mut record = TurnRecord::recovered();
        record.partial_response = Some(PartialModelResponse {
            text: "unfinished output".into(),
            ..Default::default()
        });
        repository
            .append_entries(
                &mut session,
                vec![
                    SessionEntryData::TurnStart {
                        turn_id: "t".into(),
                    },
                    SessionEntryData::TurnEnd {
                        turn_id: "t".into(),
                        record: Box::new(record),
                    },
                ],
            )
            .unwrap();
        let mut app = App::new(vec![]);
        app.load_session(&session);
        assert!(app.timeline.iter().any(|item|matches!(item,TimelineItem::Assistant {text,..} if text=="[Partial response] unfinished output")));
        assert!(
            !app.timeline.iter().any(
                |item| matches!(item,TimelineItem::Assistant {text,..} if text=="hidden branch")
            )
        );
        assert!(
            app.timeline.iter().any(
                |item| matches!(item,TimelineItem::Notice(text) if text.contains("Interrupted"))
            )
        );
    }

    #[test]
    fn input_buffer_edits_at_utf8_boundaries() {
        let mut input = InputBuffer::default();
        input.insert('你');
        input.insert('a');
        input.move_left();
        input.insert('好');
        assert_eq!(input.value(), "你好a");
        input.backspace();
        assert_eq!(input.value(), "你a");
        input.delete();
        assert_eq!(input.value(), "你");
    }

    #[test]
    fn slash_menu_tracks_filtering_selection_and_dismissal() {
        let mut app = App::new(Vec::new());
        app.input.insert('/');
        assert_eq!(app.slash_suggestions().len(), 16);
        assert_eq!(app.selected_slash_command().unwrap().name, "exit");

        app.dismiss_slash_menu();
        assert!(app.slash_suggestions().is_empty());

        app.input.insert('e');
        app.slash_input_changed();
        assert_eq!(app.selected_slash_command().unwrap().name, "exit");
    }

    #[test]
    fn reducer_aggregates_streams_and_tool_outcomes() {
        let mut app = App::new(Vec::new());
        app.apply(event(
            1,
            AgentRawEvent::ContentStarted { response_index: 0 },
        ));
        app.apply(event(
            2,
            AgentRawEvent::ContentChunk {
                response_index: 0,
                delta: Arc::from("hello"),
            },
        ));
        app.apply(event(
            3,
            AgentRawEvent::ContentEnded {
                response_index: 0,
                outcome: StreamEnd::Completed,
            },
        ));
        app.apply(event(
            4,
            AgentRawEvent::ToolExecutionStarted {
                call_id: Arc::from("call-1"),
                name: Arc::from("read_file"),
                arguments: Arc::from("{}"),
            },
        ));
        app.apply(event(
            5,
            AgentRawEvent::ToolExecutionEnded {
                artifacts: Vec::new(),
                call_id: Arc::from("call-1"),
                name: Arc::from("read_file"),
                outcome: Arc::new(ToolExecutionOutcome::Succeeded {
                    output: json!({"content": "ok"}),
                    truncated: false,
                }),
            },
        ));

        assert!(matches!(
            &app.timeline[0],
            TimelineItem::Assistant { text, open: false } if text == "hello"
        ));
        assert!(matches!(
            &app.timeline[1],
            TimelineItem::Tool { status, .. } if status == "done"
        ));
    }
}
