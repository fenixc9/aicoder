use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use aicoder_core::{
    AgentRawEvent, AgentRawEventEnvelope, AgentRunState, AgentTurnOutcome, SessionSelection,
    ToolExecutionOutcome, TurnExecutionContext,
    session::{Session, SessionInfo},
    tools::ToolInvocation,
    types::{ChatMessage, Role, Usage},
};
use tokio::sync::oneshot;
use unicode_width::UnicodeWidthStr;

use crate::commands::{self, CommandSpec};

pub enum AppEvent {
    Agent(AgentRawEventEnvelope),
    Approval {
        invocation: ToolInvocation,
        respond_to: oneshot::Sender<bool>,
    },
    TurnFinished(AgentTurnOutcome),
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
    pub respond_to: oneshot::Sender<bool>,
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
    pub sessions: Vec<SessionInfo>,
    pub selected_session: usize,
    pub session_selection: SessionSelection,
    pub timeline: Vec<TimelineItem>,
    pub input: InputBuffer,
    pub focus: Focus,
    pub state: AgentRunState,
    pub usage: Usage,
    pub round: usize,
    pub active_context: Option<TurnExecutionContext>,
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
            sessions,
            selected_session: 0,
            session_selection: SessionSelection::New,
            timeline: Vec::new(),
            input: InputBuffer::default(),
            focus: Focus::Input,
            state: AgentRunState::Idle,
            usage: Usage::default(),
            round: 0,
            active_context: None,
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

    pub fn begin_turn(&mut self, prompt: String, context: TurnExecutionContext) {
        self.timeline.push(TimelineItem::User(prompt));
        self.active_context = Some(context);
        self.started_at = Some(Instant::now());
        self.elapsed = Duration::ZERO;
        self.state = AgentRunState::Preparing;
        self.usage = Usage::default();
        self.round = 0;
        self.scroll_back = 0;
    }

    pub fn cancel(&mut self, reason: &str) {
        if let Some(context) = &self.active_context {
            context.cancel(reason);
            self.timeline
                .push(TimelineItem::Notice("Cancelling active turn...".into()));
        }
        self.approval = None;
    }

    pub fn resolve_approval(&mut self, approved: bool) {
        if let Some(approval) = self.approval.take() {
            let _ = approval.respond_to.send(approved);
        }
    }

    pub fn apply(&mut self, event: AppEvent) {
        match event {
            AppEvent::Agent(envelope) => self.apply_agent_event(envelope),
            AppEvent::Approval {
                invocation,
                respond_to,
            } => {
                self.approval = Some(PendingApproval {
                    invocation,
                    respond_to,
                });
            }
            AppEvent::TurnFinished(outcome) => self.finish_turn(outcome),
        }
    }

    fn apply_agent_event(&mut self, envelope: AgentRawEventEnvelope) {
        self.round = self.round.max(envelope.round.unwrap_or(0));
        match envelope.event {
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
                call_id, outcome, ..
            } => {
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
        match outcome {
            AgentTurnOutcome::Completed(result) => {
                self.round = result.execution_result.rounds;
                self.state = AgentRunState::Completed {
                    rounds: result.execution_result.rounds,
                };
                if let Some(session) = result.session {
                    self.session_selection = SessionSelection::Existing(session.id);
                }
                self.usage = result.execution_result.usage;
            }
            AgentTurnOutcome::Aborted(turn) => {
                self.round = turn.rounds;
                self.state = AgentRunState::Aborted {
                    round: (turn.rounds > 0).then_some(turn.rounds),
                };
                let reason = turn.error.to_string();
                if let Some(session) = turn.session {
                    self.session_selection = SessionSelection::Existing(session.id);
                }
                self.usage = turn.usage;
                if !self.timeline.iter().rev().take(2).any(
                    |item| matches!(item, TimelineItem::Notice(text) if text.contains(&reason)),
                ) {
                    self.timeline
                        .push(TimelineItem::Notice(format!("Turn cancelled: {reason}")));
                }
            }
            AgentTurnOutcome::Failed(turn) => {
                self.round = turn.rounds;
                self.state = AgentRunState::Failed {
                    round: (turn.rounds > 0).then_some(turn.rounds),
                };
                let message = format!("{:#}", turn.error);
                if let Some(session) = turn.session {
                    self.session_selection = SessionSelection::Existing(session.id);
                }
                self.usage = turn.usage;
                if !self
                    .timeline
                    .iter()
                    .rev()
                    .take(2)
                    .any(|item| matches!(item, TimelineItem::Error(text) if text == &message))
                {
                    self.timeline.push(TimelineItem::Error(message));
                }
            }
        }
        self.elapsed = self
            .started_at
            .map(|started| started.elapsed())
            .unwrap_or(self.elapsed);
        self.active_context = None;
        self.started_at = None;
        self.approval = None;
    }

    pub fn load_session(&mut self, session: &Session) {
        self.timeline.clear();
        self.tool_indexes.clear();
        for entry in session.messages() {
            push_message(&mut self.timeline, &entry.message);
        }
        self.session_selection = SessionSelection::Existing(session.metadata().id.clone());
        self.focus = Focus::Input;
        self.scroll_back = 0;
    }

    pub fn select_new_session(&mut self) {
        self.session_selection = SessionSelection::New;
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

fn push_message(timeline: &mut Vec<TimelineItem>, message: &ChatMessage) {
    if let Some(reasoning) = message.reasoning.as_deref().filter(|text| !text.is_empty()) {
        timeline.push(TimelineItem::Reasoning {
            text: reasoning.into(),
            open: false,
        });
    }
    let content = message.content.clone().unwrap_or_default();
    match message.role {
        Role::User => timeline.push(TimelineItem::User(content)),
        Role::Assistant if !content.is_empty() => timeline.push(TimelineItem::Assistant {
            text: content,
            open: false,
        }),
        Role::Tool => timeline.push(TimelineItem::Tool {
            name: message.name.clone().unwrap_or_else(|| "tool".into()),
            arguments: String::new(),
            status: "recorded".into(),
            output: Some(content),
        }),
        _ => {}
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
            run_id: RunId::new(),
            sequence,
            round: Some(1),
            event,
        })
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
        assert_eq!(app.slash_suggestions().len(), 1);
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
        app.apply(event(1, AgentRawEvent::ContentStarted { choice_index: 0 }));
        app.apply(event(
            2,
            AgentRawEvent::ContentChunk {
                choice_index: 0,
                delta: Arc::from("hello"),
            },
        ));
        app.apply(event(
            3,
            AgentRawEvent::ContentEnded {
                choice_index: 0,
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
