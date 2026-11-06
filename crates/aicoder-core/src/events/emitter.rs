use super::{
    AgentEventHandler, AgentRawEvent, EventBufferConfig, EventDeliveryReport, EventDeliveryStatus,
    EventSource, RunId, handler::dispatch_event, subscription::Observer,
};
use serde::{Deserialize, Serialize};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDeliveryConfig {
    pub callback_buffer: EventBufferConfig,
    pub callback_drain_timeout: Duration,
}
impl Default for EventDeliveryConfig {
    fn default() -> Self {
        Self {
            callback_buffer: EventBufferConfig::default(),
            callback_drain_timeout: Duration::from_secs(1),
        }
    }
}
impl EventDeliveryConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        self.callback_buffer.validate()?;
        anyhow::ensure!(
            !self.callback_drain_timeout.is_zero(),
            "callback drain timeout must be positive"
        );
        anyhow::ensure!(
            std::time::Instant::now()
                .checked_add(self.callback_drain_timeout)
                .is_some(),
            "callback drain timeout is too large"
        );
        Ok(())
    }
}
struct CallbackWorker {
    observer: Arc<Observer>,
    done: Mutex<Option<mpsc::Receiver<()>>>,
    timeout: Duration,
}
/// Ordered producer. Publication never waits for an observer; overflow is explicitly reported.
pub struct AgentEventEmitter {
    source: EventSource,
    round: Option<usize>,
    worker: Option<Arc<CallbackWorker>>,
    owner: bool,
}
impl Clone for AgentEventEmitter {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            round: self.round,
            worker: self.worker.clone(),
            owner: false,
        }
    }
}
impl AgentEventEmitter {
    pub fn new(handler: Arc<dyn AgentEventHandler>) -> Self {
        Self::with_source(
            handler,
            EventSource::new(RunId::new()),
            EventDeliveryConfig::default(),
        )
    }
    pub(crate) fn with_source(
        handler: Arc<dyn AgentEventHandler>,
        source: EventSource,
        config: EventDeliveryConfig,
    ) -> Self {
        let worker = if handler.is_noop() {
            None
        } else {
            let limits = config.callback_buffer.validate().unwrap_or_default();
            let observer = source.callback(limits);
            let target = observer.clone();
            let (done, receive) = mpsc::sync_channel(1);
            let name = source.run_id().to_string();
            let spawn = thread::Builder::new()
                .name(format!("aicoder-events-{}", &name[..8]))
                .spawn(move || {
                    while let Ok(Some(event)) = target.blocking_recv() {
                        if catch_unwind(AssertUnwindSafe(|| {
                            dispatch_event(handler.as_ref(), &event)
                        }))
                        .is_err()
                        {
                            target.fail(
                                EventDeliveryStatus::HandlerPanicked,
                                Some(event.sequence),
                                true,
                            );
                            break;
                        }
                        target.acknowledge(event.sequence);
                    }
                    let _ = done.send(());
                });
            if spawn.is_err() {
                observer.fail(EventDeliveryStatus::WorkerUnavailable, None, true);
            }
            Some(Arc::new(CallbackWorker {
                observer,
                done: Mutex::new(Some(receive)),
                timeout: if config.validate().is_ok() {
                    config.callback_drain_timeout
                } else {
                    EventDeliveryConfig::default().callback_drain_timeout
                },
            }))
        };
        Self {
            source,
            round: None,
            worker,
            owner: true,
        }
    }
    pub fn is_closed(&self) -> bool {
        self.source.is_closed()
    }
    pub fn source(&self) -> EventSource {
        self.source.clone()
    }
    pub(crate) fn round_scope(&self) -> Option<usize> {
        self.round
    }
    pub(crate) fn for_round(&self, round: usize) -> Self {
        let mut emitter = self.clone();
        emitter.round = Some(round);
        emitter
    }
    pub fn emit(&self, event: AgentRawEvent) {
        self.source.publish(self.round, event);
    }
    pub fn try_emit(&self, event: AgentRawEvent) -> bool {
        self.source.publish(self.round, event)
    }
    pub(crate) fn bind_session(&self, session_id: String, turn_id: String) {
        self.source.bind_session(session_id, turn_id);
    }
    pub(crate) fn close_round(&self, outcome: &crate::RunOutcome) {
        self.source.close_round(outcome);
    }
    /// Attach the compact summary before publishing a manually constructed terminal event.
    pub fn set_termination(&self, outcome: &crate::RunOutcome) {
        self.source.termination(outcome);
    }
    pub fn delivery_report(&self) -> EventDeliveryReport {
        self.source.report()
    }
    /// Seal publication, drain the legacy callback for a bounded time, and return observation status.
    pub async fn shutdown(&self) -> EventDeliveryReport {
        self.source.close();
        if let Some(worker) = &self.worker {
            let done = worker.done.lock().expect("callback worker poisoned").take();
            if let Some(done) = done {
                let timeout = worker.timeout;
                let result = match done.try_recv() {
                    Ok(()) => Ok(Ok(())),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Ok(Err(mpsc::RecvTimeoutError::Disconnected))
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        tokio::task::spawn_blocking(move || done.recv_timeout(timeout)).await
                    }
                };
                let status = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(mpsc::RecvTimeoutError::Timeout)) => {
                        Some(EventDeliveryStatus::CallbackTimedOut)
                    }
                    _ => Some(EventDeliveryStatus::WorkerUnavailable),
                };
                if let Some(status) = status {
                    let report = worker.observer.report();
                    let first = report
                        .last_delivered_sequence
                        .map_or(Some(report.starting_sequence), |sequence| {
                            sequence.checked_add(1)
                        });
                    worker.observer.fail(status, first, true);
                }
            }
        }
        self.source.report()
    }
}
impl Drop for AgentEventEmitter {
    fn drop(&mut self) {
        if self.owner {
            self.source.abandon();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentRawEventEnvelope;
    #[tokio::test]
    async fn callbacks_are_serialized_off_the_runtime() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let target = received.clone();
        let events = AgentEventEmitter::new(Arc::new(move |event: &AgentRawEventEnvelope| {
            thread::sleep(Duration::from_millis(25));
            target.lock().unwrap().push((
                event.sequence,
                thread::current().name().unwrap_or_default().to_string(),
            ));
        }));
        for _ in 0..4 {
            events.emit(AgentRawEvent::RoundStarted);
        }
        let report = events.shutdown().await;
        assert!(!report.has_failures());
        let received = received.lock().unwrap();
        assert_eq!(
            received.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(
            received
                .iter()
                .all(|(_, name)| name.starts_with("aicoder-events-"))
        );
    }
}
