//! Bounded, host-independent controls for one owned agent run.
mod custom;
mod input;
use crate::{
    RunId, TurnExecutionContext,
    model::{ContentBlock, Message, MessageRole},
    tools::{ApprovalDecision, ApprovalRequest, ApprovalSessionInfo, ToolInvocation},
};
pub use custom::{
    CustomDelivery, CustomSubmission, CustomSubmissionFuture, RejectedCustomSubmission,
};
pub use input::{InputSubmission, InputSubmissionFuture, RejectedInputSubmission};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    error::Error,
    fmt,
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, oneshot};

#[derive(Debug, Clone)]
pub struct RunControlConfig {
    /// Pending count, including a control currently being checkpointed.
    pub max_pending_inputs: usize,
    /// Encoded JSON bytes of one user message.
    pub max_input_bytes: usize,
    /// Sum of encoded message bytes awaiting successful application.
    pub max_pending_bytes: usize,
    /// Explicitly route this run's mutating/command approvals to the handle.
    pub handle_approvals: bool,
}
impl Default for RunControlConfig {
    fn default() -> Self {
        Self {
            max_pending_inputs: 32,
            max_input_bytes: 64 * 1024,
            max_pending_bytes: 256 * 1024,
            handle_approvals: false,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunInputKind {
    Steer,
    FollowUp,
    Context,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<crate::CustomMessageMetadata>,
    /// Ephemeral raw provenance retained for failure recovery; omitted from controls/events wire data.
    #[serde(skip)]
    pub provenance: Option<Arc<crate::extensions::InputReceipt>>,
    pub run_id: String,
    pub id: u64,
    pub kind: RunInputKind,
    pub message: Message,
}
impl RunInput {
    pub fn custom_message(&self) -> anyhow::Result<Option<crate::CustomMessage>> {
        match &self.custom {
            Some(metadata) => Ok(Some(crate::CustomMessage::from_canonical(
                metadata.clone(),
                self.message.clone(),
            )?)),
            None => {
                anyhow::ensure!(
                    self.message.role != MessageRole::Custom,
                    "custom control lacks typed metadata"
                );
                Ok(None)
            }
        }
    }
    pub fn original_message(&self) -> &Message {
        self.provenance
            .as_ref()
            .and_then(|p| p.original.as_ref())
            .unwrap_or(&self.message)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunControlStatus {
    Waiting,
    Running,
    Finishing,
    Finished,
    Dropped,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequestId {
    pub run_id: String,
    pub sequence: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRunApproval {
    pub id: ApprovalRequestId,
    pub invocation: ToolInvocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<ApprovalSessionInfo>,
}
#[derive(Debug, Clone)]
pub struct RunControlSnapshot {
    /// Latest counted projected request; no messages, credentials or backend checkpoints.
    pub model_context: Option<crate::ModelContextInfo>,
    pub run_id: RunId,
    pub session_id: Option<String>,
    pub revision: u64,
    pub status: RunControlStatus,
    pub pending_inputs: Vec<RunInput>,
    pub approvals: Vec<PendingRunApproval>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunControlError {
    Closed,
    InputPreparationRequired,
    InputBindingMismatch,
    CustomUnsupported,
    CustomBusy,
    SessionUnavailable,
    QueueFull,
    InputTooLarge,
    InvalidInput,
    UnknownApproval,
    InvalidConfig,
    InvalidApprovalDecision,
}
impl fmt::Display for RunControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed => "run no longer accepts controls",
            Self::InputPreparationRequired => "input hooks require asynchronous submit_input",
            Self::CustomBusy => "next-turn custom messages are claimed by an active preparation",
            Self::CustomUnsupported => "run journal does not support custom delivery",
            Self::SessionUnavailable => "next-turn custom delivery requires a bound session",
            Self::InputBindingMismatch => {
                "run input processor differs or raw inputs predate its installation"
            }
            Self::QueueFull => "run input queue is full",
            Self::InputTooLarge => "run input exceeds its byte limit",
            Self::InvalidInput => "control input must contain user text or images",
            Self::UnknownApproval => "approval is unknown, expired, or already answered",
            Self::InvalidConfig => "run input limits must be positive",
            Self::InvalidApprovalDecision => {
                "session authorization is unavailable for this request"
            }
        })
    }
}
impl Error for RunControlError {}
#[derive(Debug)]
pub struct RejectedRunInput {
    pub error: RunControlError,
    pub message: Message,
}
impl fmt::Display for RejectedRunInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl Error for RejectedRunInput {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}
struct Queued {
    input: RunInput,
    bytes: usize,
}
struct Approval {
    request: PendingRunApproval,
    response: oneshot::Sender<ApprovalDecision>,
}
struct State {
    custom_inbox: Option<Arc<crate::CustomInbox>>,
    custom_capable: Option<bool>,
    custom_total: usize,
    custom_bytes: usize,
    input_pipeline: Option<Arc<crate::extensions::InputPipeline>>,
    input_error: Option<RunControlError>,
    inflight: std::collections::BTreeMap<u64, usize>,
    inflight_bytes: usize,
    input_started: Option<tokio::time::Instant>,
    model_context: Option<crate::ModelContextInfo>,
    status: RunControlStatus,
    revision: u64,
    session_id: Option<String>,
    next_id: u64,
    pending: VecDeque<Queued>,
    pending_bytes: usize,
    approval: Option<Approval>,
}
pub(crate) struct ControlState {
    events: crate::events::EventSource,
    id: RunId,
    config: RunControlConfig,
    state: Mutex<State>,
    changed: Notify,
}
#[derive(Clone)]
pub struct RunHandle {
    shared: Arc<ControlState>,
    context: TurnExecutionContext,
}
/// Unique execution side. Dropping it closes controls and outstanding approval senders.
pub struct RunControl {
    pub(crate) shared: Arc<ControlState>,
    pub(crate) context: TurnExecutionContext,
}
impl RunHandle {
    pub fn pair() -> (Self, RunControl) {
        Self::channel(TurnExecutionContext::new(), RunControlConfig::default())
            .expect("default control configuration")
    }
    pub fn channel(
        parent: TurnExecutionContext,
        config: RunControlConfig,
    ) -> Result<(Self, RunControl), RunControlError> {
        if config.max_pending_inputs == 0
            || config.max_input_bytes == 0
            || config.max_pending_bytes == 0
        {
            return Err(RunControlError::InvalidConfig);
        }
        let id = RunId::new();
        let shared = Arc::new(ControlState {
            events: crate::events::EventSource::new(id),
            id,
            config,
            state: Mutex::new(State {
                custom_inbox: None,
                custom_capable: None,
                custom_total: 0,
                custom_bytes: 0,
                input_pipeline: None,
                input_error: None,
                inflight: input::empty_reservations(),
                inflight_bytes: 0,
                input_started: None,
                model_context: None,
                status: RunControlStatus::Waiting,
                revision: 0,
                session_id: None,
                next_id: 1,
                pending: VecDeque::new(),
                pending_bytes: 0,
                approval: None,
            }),
            changed: Notify::new(),
        });
        let mut context = parent.child();
        context.control = Some(shared.clone());
        Ok((
            Self {
                shared: shared.clone(),
                context: context.clone(),
            },
            RunControl { shared, context },
        ))
    }
    pub fn events(&self) -> crate::events::EventSource {
        self.shared.events.clone()
    }
    pub fn subscribe(
        &self,
        limits: crate::events::EventBufferConfig,
    ) -> Result<crate::events::EventSubscription, crate::events::SubscribeError> {
        self.shared.events.subscribe(limits)
    }
    pub fn run_id(&self) -> RunId {
        self.shared.id
    }
    pub fn input_processing_enabled(&self) -> bool {
        self.shared
            .state
            .lock()
            .expect("run controls poisoned")
            .input_pipeline
            .is_some()
    }
    pub fn steer(&self, message: Message) -> Result<u64, RejectedRunInput> {
        self.submit(RunInputKind::Steer, message)
    }
    pub fn follow_up(&self, message: Message) -> Result<u64, RejectedRunInput> {
        self.submit(RunInputKind::FollowUp, message)
    }
    fn submit(&self, kind: RunInputKind, message: Message) -> Result<u64, RejectedRunInput> {
        let result = (|| {
            if self.context.is_cancelled() {
                return Err(RunControlError::Closed);
            }
            if message.role != MessageRole::User
                || message.content.is_empty()
                || message.content.iter().any(|block| {
                    !matches!(
                        block,
                        ContentBlock::Text { .. } | ContentBlock::Image { .. }
                    )
                })
            {
                return Err(RunControlError::InvalidInput);
            }
            let mut counter = ByteCounter {
                bytes: 0,
                limit: self.shared.config.max_input_bytes,
            };
            serde_json::to_writer(&mut counter, &message)
                .map_err(|_| RunControlError::InputTooLarge)?;
            let mut state = self.shared.state.lock().expect("run controls poisoned");
            if !matches!(
                state.status,
                RunControlStatus::Waiting | RunControlStatus::Running
            ) || self.context.is_cancelled()
            {
                return Err(RunControlError::Closed);
            }
            if state
                .input_pipeline
                .as_ref()
                .is_some_and(|p| p.requires_async())
            {
                return Err(RunControlError::InputPreparationRequired);
            }
            if state.pending.len() + state.inflight.len() >= self.shared.config.max_pending_inputs
                || counter.bytes
                    > self
                        .shared
                        .config
                        .max_pending_bytes
                        .saturating_sub(state.pending_bytes.saturating_add(state.inflight_bytes))
            {
                return Err(RunControlError::QueueFull);
            }
            let id = state.next_id;
            state.next_id = state
                .next_id
                .checked_add(1)
                .ok_or(RunControlError::QueueFull)?;
            state.pending_bytes += counter.bytes;
            state.pending.push_back(Queued {
                input: RunInput {
                    custom: None,
                    provenance: None,
                    run_id: self.shared.id.to_string(),
                    id,
                    kind,
                    message: message.clone(),
                },
                bytes: counter.bytes,
            });
            self.shared.update(&mut state);
            Ok(id)
        })();
        result.map_err(|error| RejectedRunInput { error, message })
    }
    pub fn cancel(&self, reason: impl Into<Arc<str>>) -> Result<(), RunControlError> {
        let mut state = self.shared.state.lock().expect("run controls poisoned");
        if !matches!(
            state.status,
            RunControlStatus::Waiting | RunControlStatus::Running
        ) {
            return Err(RunControlError::Closed);
        }
        self.context.cancel(reason);
        state.status = RunControlStatus::Finishing;
        state.approval = None;
        self.shared.update(&mut state);
        Ok(())
    }
    /// Read count metadata without cloning queued input/approval payloads.
    pub fn model_context(&self) -> Option<crate::ModelContextInfo> {
        self.shared
            .state
            .lock()
            .expect("run controls poisoned")
            .model_context
            .clone()
    }
    pub fn snapshot(&self) -> RunControlSnapshot {
        let mut snapshot = self.shared.snapshot();
        if self.context.is_cancelled()
            && matches!(
                snapshot.status,
                RunControlStatus::Waiting | RunControlStatus::Running
            )
        {
            snapshot.status = RunControlStatus::Finishing;
        }
        snapshot
    }
    /// Wait for a newer revision, cancellation, or terminal state.
    pub async fn changed(&self, after: u64) -> RunControlSnapshot {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let snapshot = self.snapshot();
            if snapshot.revision > after
                || self.context.is_cancelled()
                || matches!(
                    snapshot.status,
                    RunControlStatus::Finished | RunControlStatus::Dropped
                )
            {
                return snapshot;
            }
            tokio::select! {_=notified=>{},_=self.context.cancelled()=>{}}
        }
    }
    pub fn respond_approval(
        &self,
        id: &ApprovalRequestId,
        approved: bool,
    ) -> Result<(), RunControlError> {
        self.respond_approval_decision(id, approved.into())
    }
    pub fn respond_approval_decision(
        &self,
        id: &ApprovalRequestId,
        decision: ApprovalDecision,
    ) -> Result<(), RunControlError> {
        let mut state = self.shared.state.lock().expect("run controls poisoned");
        if self.context.is_cancelled() || state.status != RunControlStatus::Running {
            return Err(RunControlError::Closed);
        }
        if state
            .approval
            .as_ref()
            .is_none_or(|pending| &pending.request.id != id)
        {
            return Err(RunControlError::UnknownApproval);
        }
        if decision == ApprovalDecision::AllowSession
            && state.approval.as_ref().unwrap().request.session.is_none()
        {
            return Err(RunControlError::InvalidApprovalDecision);
        }
        let pending = state.approval.take().unwrap();
        let result = pending
            .response
            .send(decision)
            .map_err(|_| RunControlError::UnknownApproval);
        self.shared.update(&mut state);
        result
    }
}
impl ControlState {
    fn update(&self, state: &mut State) {
        state.revision = state.revision.saturating_add(1);
        self.changed.notify_waiters();
    }
    pub(crate) fn snapshot(&self) -> RunControlSnapshot {
        let state = self.state.lock().expect("run controls poisoned");
        RunControlSnapshot {
            model_context: state.model_context.clone(),
            run_id: self.id,
            session_id: state.session_id.clone(),
            revision: state.revision,
            status: state.status,
            pending_inputs: state
                .pending
                .iter()
                .map(|queued| queued.input.clone())
                .collect(),
            approvals: state
                .approval
                .iter()
                .map(|approval| approval.request.clone())
                .collect(),
        }
    }
    pub(crate) fn observe_context(&self, info: &Option<crate::ModelContextInfo>) {
        let mut state = self.state.lock().expect("run controls poisoned");
        state.model_context = info.clone();
        self.update(&mut state);
    }
    pub(crate) fn event_source(&self) -> crate::events::EventSource {
        self.events.clone()
    }
    pub(crate) fn status(&self) -> RunControlStatus {
        self.state.lock().expect("run controls poisoned").status
    }
    pub(crate) fn start(&self) {
        let mut state = self.state.lock().expect("run controls poisoned");
        if state.status == RunControlStatus::Waiting {
            state
                .input_started
                .get_or_insert_with(tokio::time::Instant::now);
            state.status = RunControlStatus::Running;
            self.update(&mut state);
        }
    }
    pub(crate) fn session(&self, id: String) {
        let mut state = self.state.lock().expect("run controls poisoned");
        state.session_id = Some(id);
        self.update(&mut state);
    }
    pub(crate) fn next(&self, follow_up: bool, close: bool) -> Option<RunInput> {
        let mut state = self.state.lock().expect("run controls poisoned");
        let input = state
            .pending
            .iter()
            .find(|queued| queued.input.kind == RunInputKind::Steer)
            .or_else(|| {
                follow_up
                    .then(|| {
                        state
                            .pending
                            .iter()
                            .find(|q| q.input.kind == RunInputKind::FollowUp)
                    })
                    .flatten()
            })
            .map(|queued| queued.input.clone());
        if input.is_none()
            && close
            && !state
                .pending
                .iter()
                .any(|q| q.input.kind == RunInputKind::Context)
        {
            state.status = RunControlStatus::Finishing;
            state.approval = None;
            self.update(&mut state);
        }
        input
    }
    pub(crate) async fn steering_requested(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.has_steering() {
                return;
            }
            notified.await;
        }
    }
    pub(crate) fn max_pending_inputs(&self) -> usize {
        self.config.max_pending_inputs
    }
    pub(crate) fn has_steering(&self) -> bool {
        self.state
            .lock()
            .expect("run controls poisoned")
            .pending
            .iter()
            .any(|queued| queued.input.kind == RunInputKind::Steer)
    }
    pub(crate) fn applied(&self, id: u64) {
        let mut state = self.state.lock().expect("run controls poisoned");
        if let Some(position) = state
            .pending
            .iter()
            .position(|queued| queued.input.id == id)
        {
            let queued = state.pending.remove(position).unwrap();
            state.pending_bytes -= queued.bytes;
            self.update(&mut state);
        }
    }
    pub(crate) fn seal(&self) -> Vec<RunInput> {
        let mut state = self.state.lock().expect("run controls poisoned");
        state.status = RunControlStatus::Finishing;
        state.approval = None;
        self.update(&mut state);
        state
            .pending
            .iter()
            .map(|queued| queued.input.clone())
            .collect()
    }
    pub(crate) fn handle_approvals(&self) -> bool {
        self.config.handle_approvals
    }
    pub(crate) async fn approve(
        self: &Arc<Self>,
        approval: &ApprovalRequest<'_>,
        events: Option<&crate::events::AgentEventEmitter>,
    ) -> anyhow::Result<ApprovalDecision> {
        let (response, receive) = oneshot::channel();
        let id;
        let request;
        {
            let mut state = self.state.lock().expect("run controls poisoned");
            anyhow::ensure!(
                state.status == RunControlStatus::Running,
                "run no longer accepts approvals"
            );
            anyhow::ensure!(
                state.approval.is_none(),
                "another approval is already pending"
            );
            let sequence = state.next_id;
            state.next_id = sequence
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("approval ID overflow"))?;
            id = ApprovalRequestId {
                run_id: self.id.to_string(),
                sequence,
            };
            request = PendingRunApproval {
                id: id.clone(),
                invocation: approval.invocation.clone(),
                session: approval.session.clone(),
            };
            state.approval = Some(Approval {
                request: request.clone(),
                response,
            });
            self.update(&mut state);
        }
        if let Some(events) = events {
            events.emit(crate::AgentRawEvent::RunApprovalRequested {
                request: Arc::new(request),
            });
        }
        let _cleanup = ApprovalCleanup {
            shared: self.clone(),
            id,
        };
        Ok(receive.await.unwrap_or(ApprovalDecision::Deny))
    }
}
impl ControlState {
    pub(crate) fn finish(&self) {
        let mut state = self.state.lock().expect("run controls poisoned");
        state.status = RunControlStatus::Finished;
        state.approval = None;
        self.update(&mut state);
    }
}
impl RunControl {
    pub fn events(&self) -> crate::events::EventSource {
        self.shared.events.clone()
    }
    pub fn subscribe(
        &self,
        limits: crate::events::EventBufferConfig,
    ) -> Result<crate::events::EventSubscription, crate::events::SubscribeError> {
        self.shared.events.subscribe(limits)
    }
    pub(crate) fn finish(&self) {
        self.shared.finish();
    }
}
impl Drop for RunControl {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.status != RunControlStatus::Finished {
            self.shared.events.abandon();
            self.context.cancel("controlled run was dropped");
            state.status = RunControlStatus::Dropped;
            state.approval = None;
            self.shared.update(&mut state);
        }
    }
}
struct ApprovalCleanup {
    shared: Arc<ControlState>,
    id: ApprovalRequestId,
}
impl Drop for ApprovalCleanup {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .approval
            .as_ref()
            .is_some_and(|approval| approval.request.id == self.id)
        {
            state.approval = None;
            self.shared.update(&mut state);
        }
    }
}
struct ByteCounter {
    bytes: usize,
    limit: usize,
}
impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("input too large"));
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct SteeringRequested;
impl fmt::Display for SteeringRequested {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("steering requested before tool execution")
    }
}
impl Error for SteeringRequested {}
