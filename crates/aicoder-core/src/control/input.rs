use super::*;
use crate::{
    AgentInput,
    extensions::{InputDelivery, InputHandled, InputPipeline, InputPreparation, InputReceipt},
};
use std::collections::BTreeMap;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputSubmission {
    Queued { input_id: u64 },
    Handled { by: InputHandled },
}
#[derive(Debug)]
pub struct RejectedInputSubmission {
    pub input: AgentInput,
    pub error: anyhow::Error,
}
impl fmt::Display for RejectedInputSubmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl Error for RejectedInputSubmission {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.error.as_ref())
    }
}
pub(super) struct Ticket {
    shared: Arc<ControlState>,
    pub(super) id: u64,
    pub(super) created: tokio::time::Instant,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().expect("run controls poisoned");
        if let Some(bytes) = state.inflight.remove(&self.id) {
            state.inflight_bytes -= bytes;
            self.shared.update(&mut state);
        }
    }
}
impl Ticket {
    pub(super) async fn turn(&self) {
        loop {
            let changed = self.shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .shared
                .state
                .lock()
                .expect("run controls poisoned")
                .inflight
                .first_key_value()
                .is_some_and(|(id, _)| *id == self.id)
            {
                return;
            }
            changed.await;
        }
    }
}
pub type InputSubmissionFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<InputSubmission, RejectedInputSubmission>> + Send>,
>;
impl RunHandle {
    /// Reserve submission order now; dropping the returned future releases the reservation.
    pub fn begin_input(
        &self,
        kind: RunInputKind,
        input: AgentInput,
    ) -> Result<InputSubmissionFuture, RejectedInputSubmission> {
        if kind == RunInputKind::Context {
            return Err(RejectedInputSubmission {
                input,
                error: RunControlError::InvalidInput.into(),
            });
        }
        let (ticket, processor) = match self.reserve_input(&input) {
            Ok(plan) => plan,
            Err(error) => return Err(RejectedInputSubmission { input, error }),
        };
        let handle = self.clone();
        Ok(Box::pin(async move {
            let original = input.clone();
            handle
                .submit_reserved(kind, input, ticket, processor)
                .await
                .map_err(|error| RejectedInputSubmission {
                    input: original,
                    error,
                })
        }))
    }
    /// Transform/expand before queueing. Handled steering never signals interruption.
    pub async fn submit_input(
        &self,
        kind: RunInputKind,
        input: AgentInput,
    ) -> Result<InputSubmission, RejectedInputSubmission> {
        self.begin_input(kind, input)?.await
    }
    fn reserve_input(
        &self,
        input: &AgentInput,
    ) -> anyhow::Result<(Ticket, Option<Arc<InputPipeline>>)> {
        if self.context.is_cancelled() {
            return Err(RunControlError::Closed.into());
        }
        let bytes = crate::extensions::size(input.message(), self.shared.config.max_input_bytes)
            .map_err(|_| RunControlError::InputTooLarge)?;
        let bytes = match input.preparation_receipt() {
            Some(receipt) => crate::extensions::size(
                &(input.message(), receipt, &receipt.original),
                4 * crate::extensions::MAX_INPUT_HOOK_BYTES,
            )
            .map_err(|_| RunControlError::InputTooLarge)?,
            None => bytes,
        };
        self.reserve_payload(bytes)
    }
    pub(super) fn reserve_payload(
        &self,
        bytes: usize,
    ) -> anyhow::Result<(Ticket, Option<Arc<InputPipeline>>)> {
        if self.context.is_cancelled() {
            return Err(RunControlError::Closed.into());
        }
        let (ticket, processor) = {
            let mut state = self.shared.state.lock().expect("run controls poisoned");
            ensure_open(&state)?;
            if let Some(error) = state.input_error {
                return Err(error.into());
            }
            if state.pending.len() + state.inflight.len() >= self.shared.config.max_pending_inputs
                || bytes
                    > self
                        .shared
                        .config
                        .max_pending_bytes
                        .saturating_sub(state.pending_bytes.saturating_add(state.inflight_bytes))
            {
                return Err(RunControlError::QueueFull.into());
            }
            let id = state.next_id;
            state.next_id = id.checked_add(1).ok_or(RunControlError::QueueFull)?;
            state
                .input_started
                .get_or_insert_with(tokio::time::Instant::now);
            state.inflight.insert(id, bytes);
            state.inflight_bytes += bytes;
            self.shared.update(&mut state);
            (
                Ticket {
                    shared: self.shared.clone(),
                    id,
                    created: tokio::time::Instant::now(),
                },
                state.input_pipeline.clone(),
            )
        };
        Ok((ticket, processor))
    }

    async fn submit_reserved(
        &self,
        kind: RunInputKind,
        input: AgentInput,
        ticket: Ticket,
        processor: Option<Arc<InputPipeline>>,
    ) -> anyhow::Result<InputSubmission> {
        let started = self.shared.preparation_start(tokio::time::Instant::now());
        let duration = processor.as_ref().and_then(|p| p.budget.max_duration);
        let deadline = duration
            .map(|duration| {
                started
                    .checked_add(duration)
                    .ok_or_else(|| anyhow::anyhow!("run duration is too large"))
            })
            .transpose()?;
        let phase_deadline = ticket.created + std::time::Duration::from_secs(30);
        let parent_deadline = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        let work = async {
            ticket.turn().await;
            let delivery = match kind {
                RunInputKind::Steer => InputDelivery::Steer,
                RunInputKind::FollowUp => InputDelivery::FollowUp,
                RunInputKind::Context => return Err(RunControlError::InvalidInput.into()),
            };
            let prepared = match processor {
                Some(processor) => {
                    processor
                        .prepare(input, delivery, &self.context, started)
                        .await?
                }
                None => {
                    anyhow::ensure!(
                        input.prepared.is_none(),
                        "input snapshot belongs to another executor"
                    );
                    InputPreparation::Ready(input)
                }
            };
            let mut state = self.shared.state.lock().expect("run controls poisoned");
            ensure_open(&state)?;
            if self.context.is_cancelled() {
                return Err(RunControlError::Closed.into());
            }
            match prepared {
                InputPreparation::Handled(by) => Ok(InputSubmission::Handled { by }),
                InputPreparation::Ready(input) => {
                    let message = input.message().clone();
                    crate::extensions::size(&message, self.shared.config.max_input_bytes)
                        .map_err(|_| RunControlError::InputTooLarge)?;
                    let receipt = input.prepared.as_ref().and_then(|p| p.receipt.clone());
                    let complete = crate::extensions::size(
                        &(
                            &message,
                            &receipt,
                            receipt.as_ref().and_then(|r| r.original.as_ref()),
                        ),
                        4 * crate::extensions::MAX_INPUT_HOOK_BYTES,
                    )
                    .map_err(|_| RunControlError::InputTooLarge)?;
                    let reserved = *state
                        .inflight
                        .get(&ticket.id)
                        .expect("owned input reservation");
                    let others = state
                        .pending_bytes
                        .saturating_add(state.inflight_bytes)
                        .saturating_sub(reserved);
                    if complete > self.shared.config.max_pending_bytes.saturating_sub(others) {
                        return Err(RunControlError::QueueFull.into());
                    }
                    state.inflight.remove(&ticket.id);
                    state.inflight_bytes -= reserved;
                    state.pending_bytes += complete;
                    state.pending.push_back(Queued {
                        input: RunInput {
                            custom: None,
                            provenance: receipt.clone(),
                            run_id: self.shared.id.to_string(),
                            id: ticket.id,
                            kind,
                            message,
                        },
                        bytes: complete,
                    });
                    self.shared.update(&mut state);
                    Ok(InputSubmission::Queued {
                        input_id: ticket.id,
                    })
                }
            }
        };
        tokio::select! {biased;
            _=self.context.cancelled()=>Err(RunControlError::Closed.into()),
            _=self.shared.input_closed()=>Err(RunControlError::Closed.into()),
            _=parent_deadline=>Err(crate::BudgetExceeded{resource:crate::BudgetResource::Duration,limit:crate::run::millis(duration.unwrap_or_default()),used:crate::run::millis(started.elapsed()),requested:None}.into()),
            result=tokio::time::timeout_at(phase_deadline,work)=>result.map_err(|_|anyhow::anyhow!("input submission timed out"))?,
        }
    }
}
pub(super) fn ensure_open(state: &State) -> Result<(), RunControlError> {
    if matches!(
        state.status,
        RunControlStatus::Waiting | RunControlStatus::Running
    ) {
        Ok(())
    } else {
        Err(RunControlError::Closed)
    }
}
impl ControlState {
    pub(crate) fn preparation_start(&self, now: tokio::time::Instant) -> tokio::time::Instant {
        self.state
            .lock()
            .expect("run controls poisoned")
            .input_started
            .map_or(now, |started| started.min(now))
    }
    pub(crate) fn bind_input_pipeline(&self, pipeline: Option<Arc<InputPipeline>>) {
        let mut state = self.state.lock().expect("run controls poisoned");
        match (&state.input_pipeline, &pipeline) {
            (Some(old), Some(new)) if old.owner == new.owner => {}
            (None, None) => {}
            (None, Some(new)) => {
                if new.requires_async() && state.pending.iter().any(|q| q.input.custom.is_none()) {
                    state.input_error = Some(RunControlError::InputBindingMismatch);
                }
                state.input_pipeline = pipeline;
            }
            _ => state.input_error = Some(RunControlError::InputBindingMismatch),
        }
        self.update(&mut state);
    }
    pub(crate) fn check_input_pipeline(&self) -> anyhow::Result<()> {
        match self
            .state
            .lock()
            .expect("run controls poisoned")
            .input_error
        {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }
    pub(super) async fn input_closed(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if ensure_open(&self.state.lock().expect("run controls poisoned")).is_err() {
                return;
            }
            changed.await;
        }
    }
    pub(crate) fn input_receipt(&self, id: u64) -> Option<Arc<InputReceipt>> {
        self.state
            .lock()
            .expect("run controls poisoned")
            .pending
            .iter()
            .find(|q| q.input.id == id)
            .and_then(|q| q.input.provenance.clone())
    }
}
pub(super) fn empty_reservations() -> BTreeMap<u64, usize> {
    BTreeMap::new()
}
