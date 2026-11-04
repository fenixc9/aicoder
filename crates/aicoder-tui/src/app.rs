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

use crate::commands::{self, CommandSpec};

pub enum AppEvent {
    ReferenceReady(Box<Result<crate::runtime::CapturedInput, String>>),
    ModelSelected(Box<Result<aicoder_core::session::ModelSelectionReport, String>>),
    Agent(AgentRawEventEnvelope),
    TurnFinished(Box<AgentTurnOutcome>),
    CompactionFinished(Box<aicoder_core::summary::ManualCompactionReport>),
    ObservationFailed(String),
    CommandFinished(Box<Result<aicoder_core::extensions::CommandOutput, String>>),
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

pub use crate::editor::InputBuffer;
pub struct App {
    pub extension_ui: Option<crate::extension_ui::ExtensionUi>,
    pub run: Option<crate::runtime::ActiveRun>,
    observation_incomplete: bool,
    pub sessions: Vec<SessionInfo>,
    pub selected_session: usize,
    pub session_selection: SessionSelection,
    pub timeline: Vec<TimelineItem>,
    pub input: InputBuffer,
    pub history: crate::editor::History,
    pub theme: crate::theme::Theme,
    pub composer_width: usize,
    pub references: Vec<aicoder_runtime::references::PromptReference>,
    pub images: Vec<aicoder_runtime::images::ImageAttachment>,
    pub reference: Option<crate::runtime::ReferenceJob>,
    pub model_context: Option<aicoder_core::ModelContextInfo>,
    pub model_spec: Option<aicoder_core::model::ModelSpec>,
    pub host_window: Option<u64>,
    pub cost_usage_complete: bool,
    pub preview_omitted: bool,
    pub focus: Focus,
    pub state: AgentRunState,
    pub usage: ModelUsage,
    pub model_label: Option<String>,
    pub reasoning_effort: Option<aicoder_core::model::ReasoningEffort>,
    pub pending_reasoning: Option<Option<aicoder_core::model::ReasoningEffort>>,
    reasoning_operation: bool,
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

impl Drop for App {
    fn drop(&mut self) {
        if let Some(context) = &self.active_context {
            context.cancel("TUI owner dropped");
        }
        if let Some(handle) = &self.active_handle {
            let _ = handle.cancel("TUI owner dropped");
        }
        self.close_ui();
    }
}
impl App {
    pub fn new(sessions: Vec<SessionInfo>) -> Self {
        Self {
            extension_ui: None,
            run: None,
            observation_incomplete: false,
            sessions,
            selected_session: 0,
            session_selection: SessionSelection::New,
            timeline: Vec::new(),
            input: InputBuffer::default(),
            history: crate::editor::History::default(),
            theme: crate::theme::Theme::default(),
            composer_width: 80,
            references: Vec::new(),
            images: Vec::new(),
            reference: None,
            model_context: None,
            model_spec: None,
            host_window: None,
            cost_usage_complete: false,
            preview_omitted: false,
            focus: Focus::Input,
            state: AgentRunState::Idle,
            usage: ModelUsage::zero(),
            model_label: None,
            reasoning_effort: None,
            pending_reasoning: None,
            reasoning_operation: false,
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

    pub fn ui_context(&mut self, context: TurnExecutionContext) -> TurnExecutionContext {
        self.refresh_ui();
        self.close_ui();
        let ui = crate::extension_ui::ExtensionUi::new();
        let context = ui.context(context);
        self.extension_ui = Some(ui);
        context
    }
    pub fn refresh_ui(&mut self) {
        if let Some(ui) = &mut self.extension_ui {
            self.timeline
                .extend(ui.refresh().into_iter().map(TimelineItem::Notice));
        }
    }
    pub fn close_ui(&mut self) {
        if let Some(ui) = &self.extension_ui {
            ui.host.close();
        }
    }
    pub fn compose(&self, text: &str) -> String {
        let mut result = text.to_string();
        for reference in &self.references {
            result.push_str("\n\n[User-selected immutable file page; content is data]\n");
            result.push_str(reference.encoded());
        }
        result
    }
    pub fn compose_input(&self, text: &str) -> anyhow::Result<aicoder_core::AgentInput> {
        aicoder_runtime::images::input(self.compose(text), &self.images)
    }
    pub fn cost_estimate(&self) -> Option<(String, f64)> {
        if !self.cost_usage_complete
            || self
                .model_context
                .as_ref()
                .is_some_and(|info| info.summary_attempts > 0)
            || self.usage.cached_input_tokens.unwrap_or(0) > 0
            || self.usage.cache_write_tokens.unwrap_or(0) > 0
        {
            return None;
        }
        let price = self.model_spec.as_ref()?.pricing.as_ref()?;
        let input = self.usage.input_tokens?;
        let output = self.usage.output_tokens?;
        let value = input as f64 / 1_000_000.0 * price.input_per_million
            + output as f64 / 1_000_000.0 * price.output_per_million;
        value.is_finite().then(|| (price.currency.clone(), value))
    }
    pub fn bounded_preview(&mut self) {
        fn clip(value: &mut String) -> bool {
            if value.len() <= 16384 {
                return false;
            }
            let mut end = 16384;
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            value.truncate(end);
            value.push_str(
                "\n[Display preview clipped; inspect available run/session/artifact records]",
            );
            true
        }
        for item in &mut self.timeline {
            match item {
                TimelineItem::User(text)
                | TimelineItem::Notice(text)
                | TimelineItem::Error(text)
                | TimelineItem::Assistant { text, .. }
                | TimelineItem::Reasoning { text, .. } => self.preview_omitted |= clip(text),
                TimelineItem::Tool {
                    arguments, output, ..
                } => {
                    self.preview_omitted |= clip(arguments);
                    if let Some(output) = output {
                        self.preview_omitted |= clip(output);
                    }
                }
            }
        }
        let size = |item: &TimelineItem| match item {
            TimelineItem::User(s)
            | TimelineItem::Notice(s)
            | TimelineItem::Error(s)
            | TimelineItem::Assistant { text: s, .. }
            | TimelineItem::Reasoning { text: s, .. } => s.len(),
            TimelineItem::Tool {
                arguments, output, ..
            } => arguments.len() + output.as_ref().map_or(0, String::len),
        };
        let mut bytes = self.timeline.iter().map(size).sum::<usize>();
        let mut removed = 0;
        while self.timeline.len() > 512 || bytes > 2 * 1024 * 1024 {
            bytes -= size(&self.timeline[0]);
            self.timeline.remove(0);
            removed += 1;
        }
        if removed > 0 {
            self.preview_omitted = true;
            self.tool_indexes.retain(|_, index| {
                if *index < removed {
                    false
                } else {
                    *index -= removed;
                    true
                }
            });
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
        self.reasoning_operation = false;
        self.cost_usage_complete = false;
        self.active_context = Some(context);
        self.active_handle = None;
        self.started_at = Some(Instant::now());
        self.state = AgentRunState::Preparing;
        self.timeline
            .push(TimelineItem::Notice("Checking selected model...".into()));
    }
    pub fn begin_reasoning_selection(&mut self, context: TurnExecutionContext) {
        self.begin_model_selection(context);
        self.reasoning_operation = true;
    }
    pub fn begin_command(&mut self, context: TurnExecutionContext) {
        self.active_context = Some(context);
        self.active_handle = None;
        self.started_at = Some(Instant::now());
        self.state = AgentRunState::Preparing;
    }
    pub fn begin_compaction(&mut self, context: TurnExecutionContext) {
        self.cost_usage_complete = false;
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
        self.cost_usage_complete = false;
        self.model_context = None;
        self.round = 0;
        self.scroll_back = 0;
    }

    pub fn cancel(&mut self, reason: &str) {
        self.close_ui();
        if let Some(reference) = &self.reference {
            reference.context.cancel(reason);
        }
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

    pub fn queue_input(&mut self, kind: RunInputKind, input: aicoder_core::AgentInput) -> bool {
        let prompt = input.message().text_content();
        let Some(handle) = &self.active_handle else {
            self.input.replace(prompt);
            return false;
        };
        let message = input.into_message();
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
        if matches!(
            event,
            AppEvent::TurnFinished(_)
                | AppEvent::CompactionFinished(_)
                | AppEvent::ModelSelected(_)
                | AppEvent::CommandFinished(_)
                | AppEvent::RunDropped
        ) {
            self.close_ui();
        }
        match event {
            AppEvent::ReferenceReady(result) => {
                let cancelled = self
                    .reference
                    .as_ref()
                    .is_none_or(|job| job.context.is_cancelled());
                self.reference = None;
                if !cancelled {
                    match *result {
                        Ok(crate::runtime::CapturedInput::Text(reference)) => {
                            let total = self
                                .references
                                .iter()
                                .map(|r| r.encoded().len())
                                .sum::<usize>()
                                + reference.encoded().len();
                            if self.references.len() >= 4 || total > 64 * 1024 {
                                self.timeline.push(TimelineItem::Error(
                                    "Reference limit: four pages / 64 KiB. Detach a page first."
                                        .into(),
                                ));
                            } else {
                                self.timeline.push(TimelineItem::Notice(format!(
                                    "Attached {} ({}, captured revision {}).",
                                    reference.label(),
                                    if reference.page()["has_more"] == true {
                                        "partial page; continuation retained"
                                    } else {
                                        "complete page"
                                    },
                                    reference.page()["revision"]
                                )));
                                self.references.push(reference);
                            }
                        }
                        Ok(crate::runtime::CapturedInput::Image(image)) => {
                            let total = self
                                .images
                                .iter()
                                .map(|i| i.info().normalized_bytes)
                                .sum::<usize>()
                                + image.info().normalized_bytes;
                            if self.images.len() >= 4 || total > 1024 * 1024 {
                                self.timeline.push(TimelineItem::Error("Image limit: four images / 1 MiB normalized bytes. Detach an image first.".into()));
                            } else {
                                self.timeline.push(TimelineItem::Notice(format!(
                                    "Attached image {}",
                                    image.label()
                                )));
                                self.images.push(image);
                            }
                        }
                        Err(error) => self.timeline.push(TimelineItem::Error(error)),
                    }
                }
            }
            AppEvent::Agent(envelope) => self.apply_agent_event(envelope),
            AppEvent::TurnFinished(outcome) => self.finish_turn(*outcome),
            AppEvent::ModelSelected(report) => {
                let reasoning_operation = std::mem::take(&mut self.reasoning_operation);
                let cancelled = self
                    .active_context
                    .as_ref()
                    .is_some_and(|c| c.is_cancelled());

                self.run = None;
                self.active_context = None;
                self.active_handle = None;
                self.started_at = None;
                match *report {
                    Ok(report) => {
                        if reasoning_operation
                            && cancelled
                            && matches!(self.session_selection, SessionSelection::New)
                        {
                            self.state = AgentRunState::Idle;
                            self.timeline.push(TimelineItem::Notice(
                                "Reasoning selection cancelled.".into(),
                            ));
                            return;
                        }
                        self.reasoning_effort = report.reasoning_effort;
                        if reasoning_operation
                            && matches!(self.session_selection, SessionSelection::New)
                        {
                            self.pending_reasoning = Some(report.reasoning_effort);
                        }
                        self.model_label =
                            Some(format!("{}/{}", report.spec.provider, report.spec.id));
                        self.state = AgentRunState::Completed { rounds: 0 };
                        let action = if reasoning_operation
                            && matches!(self.session_selection, SessionSelection::New)
                        {
                            "Validated for next conversation"
                        } else {
                            "Model selected"
                        };
                        self.timeline.push(TimelineItem::Notice(format!(
                            "{action}: {}; reasoning {}; input {} tokens ({})",
                            self.model_label.as_deref().unwrap_or_default(),
                            report
                                .reasoning_effort
                                .map_or("default".into(), |v| v.to_string()),
                            report.input_token_estimate,
                            report.token_count.as_ref().map_or(
                                "legacy estimate".into(),
                                |count| format!("{:?}, {}", count.accuracy, count.method)
                            )
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
            AppEvent::CommandFinished(report) => {
                self.run = None;
                self.active_context = None;
                self.active_handle = None;
                self.started_at = None;
                self.state = AgentRunState::Idle;
                match *report {
                    Ok(aicoder_core::extensions::CommandOutput::Notice(text)) => {
                        self.timeline.push(TimelineItem::Notice(text))
                    }
                    Ok(aicoder_core::extensions::CommandOutput::Submit(_)) => {}
                    Err(error) => self.timeline.push(TimelineItem::Error(error)),
                }
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
        self.bounded_preview();
    }

    fn apply_agent_event(&mut self, envelope: AgentRawEventEnvelope) {
        if let Some(info) = envelope
            .termination
            .as_ref()
            .and_then(|t| t.model_context.as_ref())
        {
            self.reasoning_effort = info.reasoning_effort;
        }

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
        self.cost_usage_complete = outcome.progress().reported_usage
            && !outcome.progress().tokens.estimated
            && outcome.progress().auxiliary.is_empty();
        self.model_context = outcome.progress().model_context.clone();
        if let Some(info) = &outcome.progress().model_context {
            self.reasoning_effort = info.reasoning_effort;
        }

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

    pub fn load_run_session(&mut self, session: &aicoder_core::session::RunSession) {
        self.pending_reasoning = None;
        self.reasoning_effort = session
            .reasoning_selection()
            .ok()
            .flatten()
            .and_then(|c| c.effort);

        self.reference = None;
        self.references.clear();
        self.images.clear();
        self.model_context = None;
        self.cost_usage_complete = false;
        self.preview_omitted = false;
        match session {
            aicoder_core::session::RunSession::Full(session) => self.load_session(session),
            aicoder_core::session::RunSession::Window(window) => {
                self.timeline.clear();
                self.tool_indexes.clear();
                self.model_label = window
                    .selected_model()
                    .map(|(provider, model)| format!("{provider}/{model}"));
                self.usage = window.accounting().usage.clone();
                self.timeline.push(TimelineItem::Notice(format!("Showing a bounded archive tail; {} older messages remain available through archive pages.",window.source().archived_messages)));
                for entry in window.entries() {
                    if let Some(message) = entry.data.as_message() {
                        push_message(&mut self.timeline, message);
                    }
                }
                if window.unfinished_turn().is_some() {
                    self.timeline.push(TimelineItem::Notice(
                        "Previous execution is unfinished; inspect before retrying operations."
                            .into(),
                    ));
                }
                self.session_selection = SessionSelection::Existing(window.metadata().id.clone());
                self.focus = Focus::Input;
                self.scroll_back = 0;
            }
        }
    }
    pub fn load_session(&mut self, session: &Session) {
        self.reference = None;
        self.references.clear();
        self.images.clear();
        self.model_context = None;
        self.cost_usage_complete = false;
        self.preview_omitted = false;
        self.pending_reasoning = None;
        self.reasoning_effort = session
            .reasoning_selection()
            .ok()
            .flatten()
            .and_then(|c| c.effort);

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
                SessionEntryData::Message { message }
                | SessionEntryData::NativeContext { message, .. } => {
                    push_message(&mut self.timeline, message)
                }
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
        self.reference = None;
        self.references.clear();
        self.images.clear();
        self.model_context = None;
        self.model_spec = None;
        self.cost_usage_complete = false;
        self.preview_omitted = false;
        self.pending_reasoning = None;
        self.reasoning_effort = None;

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
            aicoder_core::model::ContentBlock::Image { source } => {
                timeline.push(TimelineItem::Notice(match source {
                    aicoder_core::model::ImageSource::Base64 { media_type, data } => format!(
                        "[Recorded image: {media_type}, {} base64 bytes]",
                        data.len()
                    ),
                    aicoder_core::model::ImageSource::Artifact { reference } => format!(
                        "[Tool image artifact: {}, {} bytes; preview depends on model capability]",
                        reference.id, reference.bytes
                    ),
                    aicoder_core::model::ImageSource::Url { .. } => {
                        "[Recorded image URL; not fetched by the TUI]".into()
                    }
                }))
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
            reasoning_effort: None,
            spec: aicoder_core::model::ModelSpec::new("host", "selected"),
            input_token_estimate: 30,
            output_reservation: 10,
            context_limit: Some(1000),
            needs_compaction: false,
            omitted_reasoning_blocks: 0,
            token_count: None,
            context_decision: None,
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
        assert_eq!(app.slash_suggestions().len(), 24);
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
    #[test]
    fn reasoning_previews_preserve_drafts_and_only_stage_for_the_new_conversation() {
        use aicoder_core::model::ReasoningEffort;
        let report = || aicoder_core::session::ModelSelectionReport {
            reasoning_effort: Some(ReasoningEffort::Low),
            spec: aicoder_core::model::ModelSpec::new("host", "m1"),
            input_token_estimate: 1,
            output_reservation: 1,
            context_limit: None,
            needs_compaction: false,
            omitted_reasoning_blocks: 0,
            token_count: None,
            context_decision: None,
        };
        let mut app = App::new(vec![]);
        app.input.replace("unsent draft".into());
        app.begin_reasoning_selection(TurnExecutionContext::new());
        app.apply(AppEvent::ModelSelected(Box::new(Ok(report()))));
        assert_eq!(app.pending_reasoning, Some(Some(ReasoningEffort::Low)));
        assert_eq!(app.input.value(), "unsent draft");
        assert!(
            !app.timeline
                .iter()
                .any(|v| matches!(v, TimelineItem::User(_)))
        );
        app.begin_reasoning_selection(TurnExecutionContext::new());
        app.apply(AppEvent::ModelSelected(Box::new(Err("unsupported".into()))));
        assert_eq!(app.pending_reasoning, Some(Some(ReasoningEffort::Low)));
        app.select_new_session();
        assert_eq!(app.pending_reasoning, None);
        let context = TurnExecutionContext::new();
        app.begin_reasoning_selection(context.clone());
        context.cancel("cancel preview");
        app.apply(AppEvent::ModelSelected(Box::new(Ok(report()))));
        assert_eq!(app.pending_reasoning, None);
        app.session_selection = SessionSelection::Existing("stored".into());
        app.begin_reasoning_selection(TurnExecutionContext::new());
        app.apply(AppEvent::ModelSelected(Box::new(Ok(report()))));
        assert_eq!(app.pending_reasoning, None);
        assert_eq!(app.reasoning_effort, Some(ReasoningEffort::Low));
    }
    #[test]
    fn display_bounds_and_optional_cost_never_modify_canonical_usage_or_assume_prices() {
        let mut app = App::new(vec![]);
        app.timeline.push(TimelineItem::User("界".repeat(10000)));
        for _ in 0..600 {
            app.timeline.push(TimelineItem::Notice("entry".into()));
        }
        app.bounded_preview();
        assert!(app.preview_omitted);
        assert!(app.timeline.len() <= 512);
        app.usage = aicoder_core::model::ModelUsage {
            input_tokens: Some(16),
            output_tokens: Some(8),
            total_tokens: Some(24),
            ..Default::default()
        };
        app.cost_usage_complete = true;
        assert!(app.cost_estimate().is_none());
        let mut spec = aicoder_core::model::ModelSpec::new("declared", "m1");
        spec.pricing = Some(aicoder_core::model::ModelPricing {
            currency: "USD".into(),
            input_per_million: 2.0,
            output_per_million: 4.0,
        });
        app.model_spec = Some(spec);
        assert!((app.cost_estimate().unwrap().1 - 0.000064).abs() < 1e-10);
        app.usage.cached_input_tokens = Some(2);
        assert!(app.cost_estimate().is_none());
        app.usage.cached_input_tokens = None;
        app.cost_usage_complete = false;
        assert!(app.cost_estimate().is_none());
        assert_eq!(app.usage.total_tokens, Some(24));
    }
}
