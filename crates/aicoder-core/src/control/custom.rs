use super::*;
use crate::{CustomMessage, custom_inbox::PendingCustomMessage};
use input::ensure_open;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomDelivery {
    Steer,
    FollowUp,
    TurnEnd,
    NextTurn,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CustomSubmission {
    Queued {
        input_id: u64,
        delivery: CustomDelivery,
    },
    NextTurn {
        queue_id: u64,
    },
}
#[derive(Debug)]
pub struct RejectedCustomSubmission {
    pub message: CustomMessage,
    pub error: anyhow::Error,
}
impl fmt::Display for RejectedCustomSubmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl Error for RejectedCustomSubmission {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.error.as_ref())
    }
}
pub type CustomSubmissionFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<CustomSubmission, RejectedCustomSubmission>> + Send,
    >,
>;
impl RunHandle {
    pub fn begin_custom(
        &self,
        delivery: CustomDelivery,
        message: CustomMessage,
    ) -> Result<CustomSubmissionFuture, RejectedCustomSubmission> {
        let prepare = || -> anyhow::Result<_> {
            message.validate()?;
            let bytes = crate::extensions::size(&message, self.shared.config.max_input_bytes)
                .map_err(|_| RunControlError::InputTooLarge)?;
            let state = self.shared.state.lock().expect("run controls poisoned");
            if state.custom_capable == Some(false) {
                return Err(RunControlError::CustomUnsupported.into());
            }
            drop(state);
            self.reserve_payload(bytes)
        };
        let (ticket, pipeline) = match prepare() {
            Ok(plan) => plan,
            Err(error) => return Err(RejectedCustomSubmission { message, error }),
        };
        let handle = self.clone();
        Ok(Box::pin(async move {
            let original = message.clone();
            let work = async {
                ticket.turn().await;
                let bytes = crate::extensions::size(&message, handle.shared.config.max_input_bytes)
                    .map_err(|_| RunControlError::InputTooLarge)?;
                let mut state = handle.shared.state.lock().expect("run controls poisoned");
                ensure_open(&state)?;
                if handle.context.is_cancelled() {
                    return Err(RunControlError::Closed.into());
                }
                if state.custom_capable == Some(false) {
                    return Err(RunControlError::CustomUnsupported.into());
                }
                if state.custom_total >= 256
                    || bytes > (8 * 1024 * 1024usize).saturating_sub(state.custom_bytes)
                {
                    return Err(RunControlError::QueueFull.into());
                }
                let result = if delivery == CustomDelivery::NextTurn {
                    let session = state
                        .session_id
                        .as_ref()
                        .ok_or(RunControlError::SessionUnavailable)?;
                    let inbox = state
                        .custom_inbox
                        .as_ref()
                        .ok_or(RunControlError::SessionUnavailable)?;
                    CustomSubmission::NextTurn {
                        queue_id: inbox.push(session, message, handle.shared.id, ticket.id)?,
                    }
                } else {
                    let kind = match delivery {
                        CustomDelivery::Steer => RunInputKind::Steer,
                        CustomDelivery::FollowUp => RunInputKind::FollowUp,
                        CustomDelivery::TurnEnd => RunInputKind::Context,
                        CustomDelivery::NextTurn => unreachable!(),
                    };
                    let input = RunInput {
                        custom: Some(message.metadata()),
                        provenance: None,
                        run_id: handle.shared.id.to_string(),
                        id: ticket.id,
                        kind,
                        message: message.message().clone(),
                    };
                    let complete =
                        crate::extensions::size(&input, handle.shared.config.max_input_bytes)
                            .map_err(|_| RunControlError::InputTooLarge)?;
                    let reserved = *state
                        .inflight
                        .get(&ticket.id)
                        .expect("owned custom reservation");
                    let others = state
                        .pending_bytes
                        .saturating_add(state.inflight_bytes)
                        .saturating_sub(reserved);
                    if complete
                        > handle
                            .shared
                            .config
                            .max_pending_bytes
                            .saturating_sub(others)
                    {
                        return Err(RunControlError::QueueFull.into());
                    }
                    state.inflight.remove(&ticket.id);
                    state.inflight_bytes -= reserved;
                    state.pending_bytes += complete;
                    state.pending.push_back(Queued {
                        input,
                        bytes: complete,
                    });
                    CustomSubmission::Queued {
                        input_id: ticket.id,
                        delivery,
                    }
                };
                state.custom_total += 1;
                state.custom_bytes += bytes;
                handle.shared.update(&mut state);
                Ok::<_, anyhow::Error>(result)
            };
            let start = handle.shared.preparation_start(tokio::time::Instant::now());
            let duration = pipeline.as_ref().and_then(|p| p.budget.max_duration);
            let deadline = duration
                .map(|duration| {
                    start
                        .checked_add(duration)
                        .ok_or_else(|| anyhow::anyhow!("run duration is too large"))
                })
                .transpose();
            let result = match deadline {
                Err(error) => Err(error),
                Ok(deadline) => {
                    let parent = async {
                        match deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending().await,
                        }
                    };
                    tokio::select! {biased;
                        _=handle.context.cancelled()=>Err(RunControlError::Closed.into()),
                        _=handle.shared.input_closed()=>Err(RunControlError::Closed.into()),
                        _=parent=>Err(crate::BudgetExceeded{resource:crate::BudgetResource::Duration,limit:crate::run::millis(duration.unwrap_or_default()),used:crate::run::millis(start.elapsed()),requested:None}.into()),
                        result=tokio::time::timeout_at(ticket.created+std::time::Duration::from_secs(30),work)=>result.map_err(|_|anyhow::anyhow!("custom submission timed out")).and_then(|v|v),
                    }
                }
            };
            result.map_err(|error| RejectedCustomSubmission {
                message: original,
                error,
            })
        }))
    }
    pub async fn submit_custom(
        &self,
        delivery: CustomDelivery,
        message: CustomMessage,
    ) -> Result<CustomSubmission, RejectedCustomSubmission> {
        self.begin_custom(delivery, message)?.await
    }
    pub fn next_turn_custom_messages(&self) -> Vec<PendingCustomMessage> {
        let state = self.shared.state.lock().expect("run controls poisoned");
        match (&state.custom_inbox, &state.session_id) {
            (Some(inbox), Some(session)) => inbox.pending(session),
            _ => vec![],
        }
    }
}
impl ControlState {
    pub(crate) fn bind_custom_inbox(&self, inbox: Arc<crate::CustomInbox>) {
        let mut state = self.state.lock().expect("run controls poisoned");
        if state
            .custom_inbox
            .as_ref()
            .is_some_and(|old| !Arc::ptr_eq(old, &inbox))
        {
            state.input_error = Some(RunControlError::InputBindingMismatch);
        } else {
            state.custom_inbox = Some(inbox);
        }
        self.update(&mut state);
    }
    pub(crate) fn configure_custom_delivery(&self, capable: bool) -> anyhow::Result<()> {
        let mut state = self.state.lock().expect("run controls poisoned");
        state.custom_capable = Some(capable);
        self.update(&mut state);
        if !capable && state.pending.iter().any(|p| p.input.custom.is_some()) {
            return Err(RunControlError::CustomUnsupported.into());
        }
        Ok(())
    }
    pub(crate) fn next_context(&self) -> Option<RunInput> {
        self.state
            .lock()
            .expect("run controls poisoned")
            .pending
            .iter()
            .find(|p| p.input.kind == RunInputKind::Context)
            .map(|p| p.input.clone())
    }
    pub(crate) fn has_context(&self) -> bool {
        self.next_context().is_some()
    }
}
