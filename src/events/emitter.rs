use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    thread,
};

use super::{AgentRawEvent, AgentRawEventEnvelope, RunId};
use tokio::sync::{mpsc, oneshot};

pub trait AgentRawEventHandler: Send + Sync + 'static {
    fn on_event(&self, event: &AgentRawEventEnvelope);
}

impl<F> AgentRawEventHandler for F
where
    F: Fn(&AgentRawEventEnvelope) + Send + Sync + 'static,
{
    fn on_event(&self, event: &AgentRawEventEnvelope) {
        self(event);
    }
}

#[allow(dead_code)]
pub struct NoopRawEventHandler;

impl AgentRawEventHandler for NoopRawEventHandler {
    fn on_event(&self, _event: &AgentRawEventEnvelope) {}
}

enum EventCommand {
    Event {
        round: Option<usize>,
        event: AgentRawEvent,
    },
    Shutdown(oneshot::Sender<()>),
}

/// A cheap, non-blocking producer for one serialized per-run event queue.
#[derive(Clone)]
pub(crate) struct AgentEventSink {
    sender: mpsc::UnboundedSender<EventCommand>,
    round: Option<usize>,
}

impl AgentEventSink {
    pub(crate) fn new(handler: Arc<dyn AgentRawEventHandler>) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<EventCommand>();
        let run_id = RunId::new();
        let run_id_text = run_id.to_string();
        thread::Builder::new()
            .name(format!("aicoder-events-{}", &run_id_text[..8]))
            .spawn(move || {
                let mut sequence = 1_u64;
                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        EventCommand::Event { round, event } => {
                            let envelope = AgentRawEventEnvelope {
                                run_id,
                                sequence,
                                round,
                                event,
                            };
                            sequence = sequence.saturating_add(1);
                            if catch_unwind(AssertUnwindSafe(|| handler.on_event(&envelope)))
                                .is_err()
                            {
                                tracing::error!(
                                    "Agent event handler panicked; continuing delivery"
                                );
                            }
                        }
                        EventCommand::Shutdown(acknowledge) => {
                            let _ = acknowledge.send(());
                            break;
                        }
                    }
                }
            })
            .expect("failed to start agent event dispatcher thread");
        Self {
            sender,
            round: None,
        }
    }

    pub(crate) fn for_round(&self, round: usize) -> Self {
        Self {
            sender: self.sender.clone(),
            round: Some(round),
        }
    }

    pub(crate) fn emit(&self, event: AgentRawEvent) {
        let _ = self.sender.send(EventCommand::Event {
            round: self.round,
            event,
        });
    }

    /// Waits asynchronously until every previously enqueued event was delivered, then stops the
    /// dedicated dispatcher thread. No callback runs on a Tokio worker thread.
    pub(crate) async fn shutdown(&self) {
        let (acknowledge, received) = oneshot::channel();
        if self
            .sender
            .send(EventCommand::Shutdown(acknowledge))
            .is_ok()
        {
            let _ = received.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use super::*;

    #[tokio::test]
    async fn callbacks_are_serialized_off_the_runtime() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let target = Arc::clone(&received);
        let handler = Arc::new(move |event: &AgentRawEventEnvelope| {
            thread::sleep(Duration::from_millis(25));
            target.lock().unwrap().push((
                event.sequence,
                thread::current().name().unwrap_or_default().to_string(),
            ));
        });
        let events = AgentEventSink::new(handler);

        for _ in 0..4 {
            events.emit(AgentRawEvent::RoundStarted);
        }

        events.shutdown().await;
        let received = received.lock().unwrap();
        assert_eq!(
            received
                .iter()
                .map(|(sequence, _)| *sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(
            received
                .iter()
                .all(|(_, thread)| thread.starts_with("aicoder-events-"))
        );
    }
}
