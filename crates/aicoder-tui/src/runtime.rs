use aicoder_runtime::session::JsonlSessionRepository;
use std::{path::PathBuf, sync::Arc};

use aicoder_core::{
    Agent, RunControl, SessionSelection, TurnExecutionContext,
    session::{Session, SessionInfo, SessionRepository},
    tools::{ApprovalSessions, ApprovalSnapshot},
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
    completion: RunCompletion,
}
enum RunCompletion {
    Turn(oneshot::Receiver<AgentTurnOutcome>),
    Compaction(oneshot::Receiver<aicoder_core::summary::ManualCompactionReport>),
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
        match &mut self.completion {
            RunCompletion::Compaction(receiver) => match receiver.await {
                Ok(report) => AppEvent::CompactionFinished(Box::new(report)),
                Err(_) => AppEvent::RunDropped,
            },
            RunCompletion::Turn(receiver) => match receiver.await {
                Ok(outcome) => AppEvent::TurnFinished(Box::new(outcome)),
                Err(_) => AppEvent::RunDropped,
            },
        }
    }
}

pub struct AgentRuntime {
    agent: Arc<Agent>,
    repository: Arc<JsonlSessionRepository>,
    workspace: PathBuf,
    runtime: aicoder_runtime::Runtime,
    provider: Arc<dyn aicoder_core::ModelProvider>,
    approvals: ApprovalSessions,
}

impl AgentRuntime {
    pub fn new(runtime: aicoder_runtime::Runtime) -> Result<Self> {
        let repository = Arc::new(runtime.sessions()?);
        let provider: Arc<dyn aicoder_core::ModelProvider> = Arc::new(runtime.client()?);
        let executor = runtime.executor_builder(provider.clone())?.build()?;
        let agent = Arc::new(Agent::new(executor, runtime.agent_config()));
        Ok(Self {
            agent,
            repository,
            workspace: runtime.config().workspace.clone(),
            runtime,
            provider,
            approvals: ApprovalSessions::default(),
        })
    }

    pub fn prepare_prompt(
        &self,
        prompt: &str,
    ) -> Result<aicoder_runtime::resources::PreparedPrompt> {
        self.runtime.prepare_prompt(prompt)
    }
    pub fn resource_summary(&self) -> Result<String> {
        let resources = self.runtime.resources()?;
        let manifest = resources.manifest();
        let files = manifest
            .files
            .iter()
            .filter(|file| file.active)
            .map(|file| format!("{:?}: {}", file.kind, file.path.display()))
            .collect::<Vec<_>>();
        Ok(format!(
            "Resource generation {}\n{}",
            manifest.generation,
            files.join("\n")
        ))
    }
    pub fn reload_resources(&mut self) -> Result<()> {
        let runtime = self.runtime.reload_resources()?;
        let executor = runtime.executor_builder(self.provider.clone())?.build()?;
        let agent = Arc::new(Agent::new(executor, runtime.agent_config()));
        self.approvals.clear();
        self.agent = agent;
        self.runtime = runtime;
        Ok(())
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
        self.repository.delete(id)?;
        self.approvals.remove(&self.workspace, id);
        Ok(())
    }

    pub fn execution_context(&self) -> TurnExecutionContext {
        TurnExecutionContext::new().with_approval_sessions(self.approvals.clone())
    }
    pub fn permissions(&self, selection: &SessionSelection) -> Option<ApprovalSnapshot> {
        match selection {
            SessionSelection::Existing(id) => self
                .approvals
                .get(&self.workspace, id)
                .map(|session| session.snapshot()),
            _ => None,
        }
    }
    pub fn revoke_permissions(&self, selection: &SessionSelection) -> bool {
        if let SessionSelection::Existing(id) = selection
            && let Some(session) = self.approvals.get(&self.workspace, id)
        {
            session.revoke_all();
            return true;
        }
        false
    }
    pub fn start_compaction(&self, id: String, context: TurnExecutionContext) -> Result<ActiveRun> {
        let agent = self.runtime.summary_agent(self.provider.clone())?;
        let repository = self.repository.clone();
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let report = agent
                .compact_session(repository.as_ref(), &id, context)
                .await;
            let _ = sent.send(report);
        });
        Ok(ActiveRun {
            events: None,
            completion: RunCompletion::Compaction(received),
        })
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
            completion: RunCompletion::Turn(completion),
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
            completion: RunCompletion::Turn(completion),
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
            completion: RunCompletion::Turn(completion),
        });
        assert_eq!(handle.snapshot().status, RunControlStatus::Waiting);
        assert!(
            handle
                .steer(Message::text(MessageRole::User, "still available"))
                .is_ok()
        );
        drop(control);
    }
    #[test]
    fn resource_reload_and_session_deletion_invalidate_their_authorization_handles() {
        use aicoder_runtime::{
            ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings,
            resources::ResourceSettings,
        };
        use std::collections::BTreeMap;
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let application = Runtime::load_with_environment(
            workspace.path(),
            RuntimeOverrides {
                settings: Settings {
                    resources: ResourceSettings {
                        parents: Setting::Value(false),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
            ConfigDiscovery::default(),
            Arc::new(BTreeMap::from([
                (
                    "AICODER_HOME".into(),
                    root.path().to_string_lossy().to_string(),
                ),
                ("AICODER_PROXY".into(), "disabled".into()),
            ])),
        )
        .unwrap();
        let mut runtime = AgentRuntime::new(application).unwrap();
        let session = runtime.repository.create(&runtime.workspace).unwrap();
        let id = session.metadata().id.clone();
        let scope = runtime.approvals.session(&runtime.workspace, &id).unwrap();
        std::fs::write(
            workspace.path().join("AGENTS.md"),
            "x".repeat(2 * 1024 * 1024),
        )
        .unwrap();
        assert!(runtime.reload_resources().is_err());
        assert!(!scope.snapshot().session.closed);
        std::fs::remove_file(workspace.path().join("AGENTS.md")).unwrap();
        runtime.reload_resources().unwrap();
        assert!(scope.snapshot().session.closed);
        let scope = runtime.approvals.session(&runtime.workspace, &id).unwrap();
        runtime.delete_session(&id).unwrap();
        assert!(scope.snapshot().session.closed);
        assert!(
            runtime
                .permissions(&SessionSelection::Existing(id))
                .is_none()
        );
    }
}
