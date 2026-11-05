//! Bounded, independent observation. Disconnecting an observer never cancels execution.
use super::{AgentRawEvent, AgentRawEventEnvelope, EVENT_SCHEMA_VERSION, RunId, RunTermination};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    error::Error,
    fmt,
    io::Write,
    sync::{Arc, Condvar, Mutex},
    time::Instant,
};
use tokio::sync::Notify;

pub const MAX_EVENT_SUBSCRIPTIONS: usize = 16;
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EventBufferConfig {
    pub max_events: usize,
    pub max_bytes: usize,
}
impl Default for EventBufferConfig {
    fn default() -> Self {
        Self {
            max_events: 1024,
            max_bytes: 4 * 1024 * 1024,
        }
    }
}
impl EventBufferConfig {
    pub fn validate(self) -> Result<Self, SubscribeError> {
        if self.max_events == 0 || self.max_bytes == 0 {
            Err(SubscribeError::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscribeError {
    InvalidLimits,
    TooManySubscriptions,
    RunClosed,
}
impl fmt::Display for SubscribeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidLimits => "event buffer limits must be positive",
            Self::TooManySubscriptions => "run subscription limit reached",
            Self::RunClosed => "run event source is closed",
        })
    }
}
impl Error for SubscribeError {}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventDeliveryStatus {
    Active,
    Draining,
    Completed,
    ClientClosed,
    Lagged,
    EventTooLarge,
    HandlerPanicked,
    WorkerUnavailable,
    CallbackTimedOut,
    Abandoned,
    SequenceExhausted,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventStreamError {
    pub status: EventDeliveryStatus,
    pub observer_id: u64,
    pub first_missing_sequence: Option<u64>,
    pub last_delivered_sequence: Option<u64>,
}
impl fmt::Display for EventStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event observer {} ended with {:?}; first missing sequence {:?}",
            self.observer_id, self.status, self.first_missing_sequence
        )
    }
}
impl Error for EventStreamError {}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserverDeliveryReport {
    pub observer_id: u64,
    pub callback: bool,
    pub status: EventDeliveryStatus,
    pub starting_sequence: u64,
    pub last_enqueued_sequence: Option<u64>,
    pub last_delivered_sequence: Option<u64>,
    pub first_missing_sequence: Option<u64>,
    pub queued_events: usize,
    pub queued_bytes: usize,
    pub peak_events: usize,
    pub peak_bytes: usize,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventDeliveryReport {
    pub observers: Vec<ObserverDeliveryReport>,
}
impl EventDeliveryReport {
    pub fn has_failures(&self) -> bool {
        self.observers.iter().any(|observer| {
            !matches!(
                observer.status,
                EventDeliveryStatus::Active
                    | EventDeliveryStatus::Draining
                    | EventDeliveryStatus::Completed
            )
        })
    }
}
struct Buffered {
    event: Arc<AgentRawEventEnvelope>,
    bytes: usize,
}
struct ObserverState {
    queue: VecDeque<Buffered>,
    report: ObserverDeliveryReport,
    closed: bool,
    error_returned: bool,
    receiver_alive: bool,
    in_flight: bool,
    discarded: bool,
}
pub(crate) struct Observer {
    limits: EventBufferConfig,
    state: Mutex<ObserverState>,
    changed: Notify,
    blocking: Condvar,
}
impl Observer {
    fn new(id: u64, callback: bool, limits: EventBufferConfig, start: u64) -> Self {
        Self {
            limits,
            state: Mutex::new(ObserverState {
                queue: VecDeque::new(),
                report: ObserverDeliveryReport {
                    observer_id: id,
                    callback,
                    status: EventDeliveryStatus::Active,
                    starting_sequence: start,
                    last_enqueued_sequence: None,
                    last_delivered_sequence: None,
                    first_missing_sequence: None,
                    queued_events: 0,
                    queued_bytes: 0,
                    peak_events: 0,
                    peak_bytes: 0,
                },
                closed: false,
                error_returned: false,
                receiver_alive: true,
                in_flight: false,
                discarded: false,
            }),
            changed: Notify::new(),
            blocking: Condvar::new(),
        }
    }
    fn notify(&self) {
        self.changed.notify_waiters();
        self.blocking.notify_all();
    }
    fn active(&self) -> bool {
        let state = self.state.lock().expect("event observer poisoned");
        !state.closed && state.receiver_alive
    }
    fn push(&self, event: Arc<AgentRawEventEnvelope>, bytes: usize) {
        let mut state = self.state.lock().expect("event observer poisoned");
        if state.closed || !state.receiver_alive {
            return;
        }
        let failure = if bytes > self.limits.max_bytes {
            Some(EventDeliveryStatus::EventTooLarge)
        } else if state.queue.len() >= self.limits.max_events
            || bytes
                > self
                    .limits
                    .max_bytes
                    .saturating_sub(state.report.queued_bytes)
        {
            Some(EventDeliveryStatus::Lagged)
        } else {
            None
        };
        if let Some(status) = failure {
            state.closed = true;
            state.report.status = status;
            state.report.first_missing_sequence = Some(event.sequence);
        } else {
            state.report.last_enqueued_sequence = Some(event.sequence);
            state.report.queued_bytes += bytes;
            state.queue.push_back(Buffered { event, bytes });
            state.report.queued_events = state.queue.len();
            state.report.peak_events = state.report.peak_events.max(state.queue.len());
            state.report.peak_bytes = state.report.peak_bytes.max(state.report.queued_bytes);
        }
        self.notify();
    }
    pub(crate) fn report(&self) -> ObserverDeliveryReport {
        self.state
            .lock()
            .expect("event observer poisoned")
            .report
            .clone()
    }
    pub(crate) fn fail(
        &self,
        status: EventDeliveryStatus,
        first_missing: Option<u64>,
        discard: bool,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(
            state.report.status,
            EventDeliveryStatus::Active | EventDeliveryStatus::Draining
        ) {
            state.report.status = status;
            state.report.first_missing_sequence = first_missing;
        }
        state.closed = true;
        if discard {
            state.discarded = true;
            let queued = state.queue.front().map(|item| item.event.sequence);
            let in_flight = state.in_flight.then(|| {
                state
                    .report
                    .last_delivered_sequence
                    .map_or(state.report.starting_sequence, |seq| seq.saturating_add(1))
            });
            state.report.first_missing_sequence =
                [state.report.first_missing_sequence, queued, in_flight]
                    .into_iter()
                    .flatten()
                    .min();
            state.queue = VecDeque::new();
            state.report.queued_events = 0;
            state.report.queued_bytes = 0;
        }
        self.notify();
    }
    fn close(&self) {
        let mut state = self.state.lock().expect("event observer poisoned");
        if !state.closed {
            state.closed = true;
            state.report.status = if state.queue.is_empty() && !state.in_flight {
                EventDeliveryStatus::Completed
            } else {
                EventDeliveryStatus::Draining
            };
        }
        self.notify();
    }
    fn take(state: &mut ObserverState, ack: bool) -> Option<Arc<AgentRawEventEnvelope>> {
        let item = state.queue.pop_front()?;
        state.report.queued_bytes -= item.bytes;
        state.report.queued_events = state.queue.len();
        if ack {
            Self::ack(state, item.event.sequence);
        }
        Some(item.event)
    }
    fn ack(state: &mut ObserverState, sequence: u64) {
        state.in_flight = false;
        state.report.last_delivered_sequence = Some(sequence);
        if state.closed
            && state.queue.is_empty()
            && state.report.status == EventDeliveryStatus::Draining
        {
            state.report.status = EventDeliveryStatus::Completed;
        }
    }
    pub(crate) fn acknowledge(&self, sequence: u64) {
        let mut state = self.state.lock().expect("event observer poisoned");
        if state.discarded {
            state.in_flight = false;
            return;
        }
        Self::ack(&mut state, sequence);
    }
    fn end(
        state: &mut ObserverState,
    ) -> Result<Option<Arc<AgentRawEventEnvelope>>, EventStreamError> {
        if state.error_returned
            || matches!(
                state.report.status,
                EventDeliveryStatus::Completed | EventDeliveryStatus::Draining
            )
        {
            return Ok(None);
        }
        state.error_returned = true;
        Err(EventStreamError {
            status: state.report.status,
            observer_id: state.report.observer_id,
            first_missing_sequence: state.report.first_missing_sequence,
            last_delivered_sequence: state.report.last_delivered_sequence,
        })
    }
    pub(crate) fn blocking_recv(
        &self,
    ) -> Result<Option<Arc<AgentRawEventEnvelope>>, EventStreamError> {
        let mut state = self.state.lock().expect("event observer poisoned");
        loop {
            if let Some(event) = Self::take(&mut state, false) {
                state.in_flight = true;
                return Ok(Some(event));
            }
            if state.closed {
                return Self::end(&mut state);
            }
            state = self.blocking.wait(state).expect("event observer poisoned");
        }
    }
}
/// One consumer of a contiguous event prefix, followed by clean EOF or one explicit error.
pub struct EventSubscription {
    pub(crate) observer: Arc<Observer>,
}
impl EventSubscription {
    pub fn report(&self) -> ObserverDeliveryReport {
        self.observer.report()
    }
    pub async fn recv(&mut self) -> Result<Option<Arc<AgentRawEventEnvelope>>, EventStreamError> {
        loop {
            let notified = self.observer.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.observer.state.lock().expect("event observer poisoned");
                if let Some(event) = Observer::take(&mut state, true) {
                    return Ok(Some(event));
                }
                if state.closed {
                    return Observer::end(&mut state);
                }
            }
            notified.await;
        }
    }
}
impl Drop for EventSubscription {
    fn drop(&mut self) {
        let mut state = self
            .observer
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.receiver_alive = false;
        state.queue = VecDeque::new();
        state.report.queued_events = 0;
        state.report.queued_bytes = 0;
        if matches!(
            state.report.status,
            EventDeliveryStatus::Active | EventDeliveryStatus::Draining
        ) {
            state.report.status = EventDeliveryStatus::ClientClosed;
        }
        state.closed = true;
        self.observer.notify();
    }
}
struct Hub {
    run_id: RunId,
    session_id: Option<Arc<str>>,
    turn_id: Option<Arc<str>>,
    started: Instant,
    next_sequence: Option<u64>,
    closed: bool,
    observers: Vec<Arc<Observer>>,
    subscriptions: usize,
    open_round: Option<usize>,
    termination: Option<Arc<RunTermination>>,
}
#[derive(Clone)]
pub struct EventSource {
    inner: Arc<Mutex<Hub>>,
}
impl EventSource {
    pub(crate) fn new(run_id: RunId) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Hub {
                run_id,
                session_id: None,
                turn_id: None,
                started: Instant::now(),
                next_sequence: Some(1),
                closed: false,
                observers: Vec::new(),
                subscriptions: 0,
                open_round: None,
                termination: None,
            })),
        }
    }
    pub fn is_closed(&self) -> bool {
        self.inner.lock().expect("event source poisoned").closed
    }
    pub fn run_id(&self) -> RunId {
        self.inner.lock().expect("event source poisoned").run_id
    }
    pub fn subscribe(
        &self,
        limits: EventBufferConfig,
    ) -> Result<EventSubscription, SubscribeError> {
        let limits = limits.validate()?;
        let mut hub = self.inner.lock().expect("event source poisoned");
        if hub.closed {
            return Err(SubscribeError::RunClosed);
        }
        if hub.subscriptions >= MAX_EVENT_SUBSCRIPTIONS {
            return Err(SubscribeError::TooManySubscriptions);
        }
        hub.subscriptions += 1;
        let observer = Arc::new(Observer::new(
            hub.subscriptions as u64,
            false,
            limits,
            hub.next_sequence.unwrap_or(u64::MAX),
        ));
        hub.observers.push(observer.clone());
        Ok(EventSubscription { observer })
    }
    pub fn report(&self) -> EventDeliveryReport {
        let hub = self.inner.lock().expect("event source poisoned");
        EventDeliveryReport {
            observers: hub
                .observers
                .iter()
                .map(|observer| observer.report())
                .collect(),
        }
    }
    pub(crate) fn callback(&self, limits: EventBufferConfig) -> Arc<Observer> {
        let mut hub = self.inner.lock().expect("event source poisoned");
        let observer = Arc::new(Observer::new(
            0,
            true,
            limits,
            hub.next_sequence.unwrap_or(u64::MAX),
        ));
        hub.observers.push(observer.clone());
        observer
    }
    pub(crate) fn bind_session(&self, session_id: String, turn_id: String) {
        let mut hub = self.inner.lock().expect("event source poisoned");
        if hub.closed {
            return;
        }
        hub.session_id = Some(session_id.into());
        hub.turn_id = Some(turn_id.into());
        let event = AgentRawEvent::SessionBound {
            session_id: hub.session_id.clone().unwrap(),
            turn_id: hub.turn_id.clone().unwrap(),
        };
        Self::publish_locked(&mut hub, None, event);
    }
    pub(crate) fn termination(&self, outcome: &crate::RunOutcome) {
        let mut hub = self.inner.lock().expect("event source poisoned");
        hub.termination = Some(Arc::new(RunTermination::from(outcome)));
    }
    pub(crate) fn close_round(&self, outcome: &crate::RunOutcome) {
        let mut hub = self.inner.lock().expect("event source poisoned");
        if let Some(round) = hub.open_round {
            let outcome = match outcome.stop() {
                Some(crate::RunStop::Cancelled { .. }) => super::RoundOutcome::Aborted,
                Some(
                    stop @ (crate::RunStop::Budget(_)
                    | crate::RunStop::Model(_)
                    | crate::RunStop::InputHandled(_)),
                ) => super::RoundOutcome::Stopped {
                    stop: Arc::new(stop.clone()),
                },
                Some(crate::RunStop::Failed(_)) => super::RoundOutcome::Failed,
                None => super::RoundOutcome::FinalAnswer,
            };
            Self::publish_locked(
                &mut hub,
                Some(round),
                AgentRawEvent::RoundCompleted { outcome },
            );
        }
    }
    pub(crate) fn publish(&self, round: Option<usize>, event: AgentRawEvent) -> bool {
        let mut hub = self.inner.lock().expect("event source poisoned");
        Self::publish_locked(&mut hub, round, event)
    }
    fn publish_locked(hub: &mut Hub, round: Option<usize>, event: AgentRawEvent) -> bool {
        if hub.closed {
            return false;
        }
        let Some(sequence) = hub.next_sequence else {
            hub.closed = true;
            for observer in &hub.observers {
                observer.fail(EventDeliveryStatus::SequenceExhausted, None, false);
            }
            return false;
        };
        hub.next_sequence = sequence.checked_add(1);
        match &event {
            AgentRawEvent::RoundStarted => hub.open_round = round,
            AgentRawEvent::RoundCompleted { .. } if hub.open_round == round => {
                hub.open_round = None
            }
            _ => {}
        }
        if sequence == 1 && matches!(&event, AgentRawEvent::AgentStarted { .. }) {
            hub.started = Instant::now();
        }
        let terminal = event.is_terminal();
        let envelope = Arc::new(AgentRawEventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            run_id: hub.run_id,
            session_id: hub.session_id.clone(),
            turn_id: hub.turn_id.clone(),
            elapsed_ms: hub
                .started
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            termination: terminal.then(|| hub.termination.clone()).flatten(),
            sequence,
            round,
            event,
        });
        let maximum = hub
            .observers
            .iter()
            .filter(|observer| observer.active())
            .map(|observer| observer.limits.max_bytes)
            .max();
        if let Some(maximum) = maximum {
            let mut size = SizeCounter {
                bytes: 0,
                limit: maximum,
            };
            let result = serde_json::to_writer(&mut size, envelope.as_ref());
            if result.is_ok() {
                for observer in &hub.observers {
                    observer.push(envelope.clone(), size.bytes);
                }
            } else {
                for observer in &hub.observers {
                    if observer.active() {
                        observer.fail(EventDeliveryStatus::EventTooLarge, Some(sequence), false);
                    }
                }
            }
        }
        if terminal {
            hub.closed = true;
            for observer in &hub.observers {
                observer.close();
            }
        }
        true
    }
    pub(crate) fn close(&self) {
        let mut hub = self.inner.lock().expect("event source poisoned");
        if !hub.closed {
            hub.closed = true;
            for observer in &hub.observers {
                observer.close();
            }
        }
    }
    pub(crate) fn abandon(&self) {
        let mut hub = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !hub.closed {
            hub.closed = true;
            for observer in &hub.observers {
                observer.fail(EventDeliveryStatus::Abandoned, hub.next_sequence, true);
            }
        }
    }
}
struct SizeCounter {
    bytes: usize,
    limit: usize,
}
impl Write for SizeCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("event byte limit"));
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
