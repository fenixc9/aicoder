use std::{path::PathBuf, sync::Arc};

use aicoder_core::{
    Agent, AgentConfig, AgentEventHandler, AgentRawEventEnvelope, ChatClient, ContextWindowConfig,
    PruningContextCompactor, SessionSelection, TurnExecutionConfig, TurnExecutionContext,
    TurnExecutor,
    session::{JsonlSessionRepository, Session, SessionInfo, SessionRepository},
    tools::{ApprovalHandler, ToolInvocation},
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::app::AppEvent;

pub struct EventForwarder {
    sender: mpsc::UnboundedSender<AppEvent>,
}

impl EventForwarder {
    pub fn new(sender: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self { sender }
    }
}

impl AgentEventHandler for EventForwarder {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        let _ = self.sender.send(AppEvent::Agent(event.clone()));
    }
}

pub struct TuiApproval {
    sender: mpsc::UnboundedSender<AppEvent>,
}

impl TuiApproval {
    pub fn new(sender: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self { sender }
    }
}

#[async_trait]
impl ApprovalHandler for TuiApproval {
    async fn approve(&self, invocation: &ToolInvocation) -> Result<bool> {
        let (respond_to, response) = oneshot::channel();
        self.sender
            .send(AppEvent::Approval {
                invocation: invocation.clone(),
                respond_to,
            })
            .map_err(|_| anyhow::anyhow!("TUI event loop closed during tool approval"))?;
        Ok(response.await.unwrap_or(false))
    }
}

pub struct AgentRuntime {
    agent: Arc<Agent>,
    repository: Arc<JsonlSessionRepository>,
    workspace: PathBuf,
    sender: mpsc::UnboundedSender<AppEvent>,
}

impl AgentRuntime {
    pub fn new(
        workspace: PathBuf,
        session_root: PathBuf,
        model: String,
        context_window: Option<usize>,
        sender: mpsc::UnboundedSender<AppEvent>,
    ) -> Result<Self> {
        let repository = Arc::new(JsonlSessionRepository::new(session_root)?);
        let client = ChatClient::from_env(&model)?;
        let mut builder = TurnExecutor::builder(client)
            .workspace(&workspace)
            .config(TurnExecutionConfig::default())
            .approval(TuiApproval::new(sender.clone()));
        if let Some(max_context_tokens) = context_window {
            builder =
                builder.context_compactor(PruningContextCompactor::new(ContextWindowConfig {
                    max_context_tokens,
                    reserved_output_tokens: 4_096,
                    preserve_recent_tokens: 8_192.min(max_context_tokens.saturating_sub(4_097)),
                })?);
        }
        let agent = Arc::new(Agent::new(
            builder.build()?,
            AgentConfig {
                model,
                system_prompt: Some(
                    "You are a coding assistant. Inspect the project before changing it, use tools when needed, verify changes, and report concisely. Reply in the user's language."
                        .into(),
                ),
                temperature: Some(0.2),
                top_p: Some(1.0),
                max_tokens: Some(4096),
                seed: None,
                stop: None,
                response_format: None,
            },
        ));
        Ok(Self {
            agent,
            repository,
            workspace,
            sender,
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
        context: TurnExecutionContext,
    ) {
        let agent = Arc::clone(&self.agent);
        let repository = Arc::clone(&self.repository);
        let sender = self.sender.clone();
        let handler = Arc::new(EventForwarder::new(sender.clone()));
        tokio::spawn(async move {
            let outcome = agent
                .run_with_session_outcome(repository.as_ref(), selection, prompt, handler, context)
                .await;
            let _ = sender.send(AppEvent::TurnFinished(outcome));
        });
    }
}
