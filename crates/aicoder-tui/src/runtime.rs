use std::{path::PathBuf, sync::Arc};

use aicoder_core::{
    Agent, RunControl, SessionSelection,
    session::{JsonlSessionRepository, Session, SessionInfo, SessionRepository},
};
use aicoder_core::{
    AgentTurnOutcome,
    events::{EventBufferConfig, EventSubscription},
};
use anyhow::{Result, ensure};
use tokio::sync::oneshot;

use crate::app::AppEvent;

pub struct ActiveRun {
    events: Option<EventSubscription>,
    completion: oneshot::Receiver<AgentTurnOutcome>,
}
impl ActiveRun {
    pub async fn recv(&mut self) -> AppEvent {
        if let Some(events) = &mut self.events {
            match events.recv().await {
                Ok(Some(event)) => return AppEvent::Agent((*event).clone()),
                Err(error) => {
                    self.events = None;
                    return AppEvent::ObservationFailed(error.to_string());
                }
                Ok(None) => self.events = None,
            }
        }
        match (&mut self.completion).await {
            Ok(outcome) => AppEvent::TurnFinished(Box::new(outcome)),
            Err(_) => AppEvent::RunDropped,
        }
    }
}

pub struct AgentRuntime {
    agent: Arc<Agent>,
    repository: Arc<JsonlSessionRepository>,
    workspace: PathBuf,
}

impl AgentRuntime {
    pub fn new(runtime: aicoder_runtime::Runtime) -> Result<Self> {
        let repository = Arc::new(runtime.sessions()?);
        let executor = runtime
            .executor_builder(Arc::new(runtime.client()?))?
            .build()?;
        let agent = Arc::new(Agent::new(executor, runtime.agent_config()));
        Ok(Self {
            agent,
            repository,
            workspace: runtime.config().workspace.clone(),
        })
    }

    pub fn sessions(&self) -> Result<Vec<SessionInfo>> {
        self.repository.list(&self.workspace)
    }

    pub fn open_session(&self, id: &str) -> Result<Session> {
        let session = self.repository.open(id)?;
        ensure!(
            session.metadata().cwd == self.workspace,
            "Session {} belongs to workspace {}, not {}",
            session.metadata().id,
            session.metadata().cwd.display(),
            self.workspace.display()
        );
        Ok(session)
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.repository.delete(id)
    }

    pub fn start_turn(
        &self,
        selection: SessionSelection,
        prompt: String,
        control: RunControl,
    ) -> Result<ActiveRun> {
        let events = control.subscribe(EventBufferConfig::default())?;
        let agent = self.agent.clone();
        let repository = self.repository.clone();
        let (sender, completion) = oneshot::channel();
        tokio::spawn(async move {
            let outcome = agent
                .run_with_session_controlled(
                    repository.as_ref(),
                    selection,
                    prompt,
                    Arc::new(()),
                    control,
                )
                .await;
            let _ = sender.send(outcome);
        });
        Ok(ActiveRun {
            events: Some(events),
            completion,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicoder_core::{
        AgentRawEvent, AgentTurnResult, RunControlStatus, RunHandle, RunProgress,
        TurnExecutionResult,
        events::{AgentEventEmitter, EventBufferConfig},
        model::{Message, MessageRole, StopReason},
    };
    #[tokio::test]
    async fn incomplete_observation_is_reported_before_the_authoritative_result() {
        let events = AgentEventEmitter::new(Arc::new(()));
        let stream = events
            .source()
            .subscribe(EventBufferConfig {
                max_events: 1,
                max_bytes: 4096,
            })
            .unwrap();
        for _ in 0..3 {
            events.emit(AgentRawEvent::ContentChunk {
                response_index: 0,
                delta: "part".into(),
            });
        }
        events.shutdown().await;
        let (sender, completion) = oneshot::channel();
        sender
            .send(AgentTurnOutcome::Completed(AgentTurnResult {
                execution_result: TurnExecutionResult {
                    final_message: Message::text(MessageRole::Assistant, "full result"),
                    finish_reason: Some(StopReason::Complete),
                    progress: RunProgress::new(vec![]),
                },
                session: None,
            }))
            .unwrap();
        let mut active = ActiveRun {
            events: Some(stream),
            completion,
        };
        assert!(matches!(active.recv().await,AppEvent::Agent(event) if event.sequence==1));
        assert!(matches!(
            active.recv().await,
            AppEvent::ObservationFailed(_)
        ));
        assert!(matches!(active.recv().await, AppEvent::TurnFinished(_)));
    }
    #[tokio::test]
    async fn dropping_the_ui_observation_does_not_cancel_the_execution_owner() {
        let (handle, control) = RunHandle::pair();
        let events = control.subscribe(EventBufferConfig::default()).unwrap();
        let (_sender, completion) = oneshot::channel();
        drop(ActiveRun {
            events: Some(events),
            completion,
        });
        assert_eq!(handle.snapshot().status, RunControlStatus::Waiting);
        assert!(
            handle
                .steer(Message::text(MessageRole::User, "still available"))
                .is_ok()
        );
        drop(control);
    }
}
