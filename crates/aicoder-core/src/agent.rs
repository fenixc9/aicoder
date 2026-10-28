//! Session-aware request construction around the same structured execution outcome.
use crate::{
    AgentEventHandler, TurnExecutionContext, TurnExecutionResult, TurnExecutor,
    model::{Message, MessageRole, ModelOptions, ModelRequest},
    run::{InterruptedTurn, RunFailure, RunOutcome, RunProgress, RunStage, RunStop},
    session::{Session, SessionMetadata, SessionRepository},
};
use anyhow::{Result, ensure};
use std::{ops::Deref, sync::Arc};
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    pub system_prompt: Option<String>,
    pub options: ModelOptions,
}
impl AgentConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system_prompt: None,
            options: ModelOptions::default(),
        }
    }
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSelection {
    New,
    ContinueMostRecent,
    Existing(String),
}
#[derive(Debug, Clone)]
pub struct AgentTurnResult {
    pub execution_result: TurnExecutionResult,
    pub session: Option<SessionMetadata>,
}
#[derive(Debug)]
pub struct InterruptedAgentTurn {
    pub session: Option<SessionMetadata>,
    pub progress: RunProgress,
    pub stop: RunStop,
    pub error: anyhow::Error,
}
impl Deref for InterruptedAgentTurn {
    type Target = RunProgress;
    fn deref(&self) -> &RunProgress {
        &self.progress
    }
}
#[derive(Debug)]
pub enum AgentTurnOutcome {
    Completed(AgentTurnResult),
    Aborted(InterruptedAgentTurn),
    Limited(InterruptedAgentTurn),
    Stopped(InterruptedAgentTurn),
    Failed(InterruptedAgentTurn),
}
impl AgentTurnOutcome {
    pub fn session(&self) -> Option<&SessionMetadata> {
        match self {
            Self::Completed(result) => result.session.as_ref(),
            Self::Aborted(turn)
            | Self::Limited(turn)
            | Self::Stopped(turn)
            | Self::Failed(turn) => turn.session.as_ref(),
        }
    }
    pub fn progress(&self) -> &RunProgress {
        match self {
            Self::Completed(result) => &result.execution_result.progress,
            Self::Aborted(turn)
            | Self::Limited(turn)
            | Self::Stopped(turn)
            | Self::Failed(turn) => &turn.progress,
        }
    }
    pub fn into_result(self) -> Result<AgentTurnResult> {
        match self {
            Self::Completed(result) => Ok(result),
            Self::Aborted(turn)
            | Self::Limited(turn)
            | Self::Stopped(turn)
            | Self::Failed(turn) => Err(turn.error.context(InterruptedTurn {
                stop: turn.stop,
                progress: turn.progress,
            })),
        }
    }
}
pub struct Agent {
    turn_executor: TurnExecutor,
    config: AgentConfig,
}
impl Agent {
    pub fn new(turn_executor: TurnExecutor, config: AgentConfig) -> Self {
        Self {
            turn_executor,
            config,
        }
    }
    pub fn turn_executor(&self) -> &TurnExecutor {
        &self.turn_executor
    }
    pub async fn run(
        &self,
        prompt: impl Into<String>,
        handler: Arc<dyn AgentEventHandler>,
    ) -> Result<AgentTurnResult> {
        self.run_with_context(prompt, handler, TurnExecutionContext::new())
            .await
    }
    pub async fn run_with_context(
        &self,
        prompt: impl Into<String>,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
    ) -> Result<AgentTurnResult> {
        self.run_outcome(prompt, handler, context)
            .await
            .into_result()
    }
    pub async fn run_outcome(
        &self,
        prompt: impl Into<String>,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
    ) -> AgentTurnOutcome {
        let mut messages = self.system_messages();
        messages.push(Message::text(MessageRole::User, prompt));
        from_execution(
            self.turn_executor
                .run_outcome(self.completion_request(messages), handler, context)
                .await,
            None,
        )
    }
    pub async fn run_with_session<R: SessionRepository>(
        &self,
        repository: &R,
        selection: SessionSelection,
        prompt: impl Into<String>,
        handler: Arc<dyn AgentEventHandler>,
    ) -> Result<AgentTurnResult> {
        self.run_with_session_context(
            repository,
            selection,
            prompt,
            handler,
            TurnExecutionContext::new(),
        )
        .await
    }
    pub async fn run_with_session_context<R: SessionRepository>(
        &self,
        repository: &R,
        selection: SessionSelection,
        prompt: impl Into<String>,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
    ) -> Result<AgentTurnResult> {
        self.run_with_session_outcome(repository, selection, prompt, handler, context)
            .await
            .into_result()
    }
    pub async fn run_with_session_outcome<R: SessionRepository>(
        &self,
        repository: &R,
        selection: SessionSelection,
        prompt: impl Into<String>,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
    ) -> AgentTurnOutcome {
        let user = Message::text(MessageRole::User, prompt);
        let mut initial = self.system_messages();
        initial.push(user.clone());
        if context.is_cancelled() {
            return interrupted(
                RunStop::Cancelled {
                    reason: context.reason().to_string(),
                },
                RunProgress::new(initial),
                None,
            );
        }
        let workspace = self.turn_executor.workspace_root();
        let mut session = match select_session(repository, workspace, selection) {
            Ok(session) => session,
            Err(error) => return failed(error, RunProgress::new(initial), None),
        };
        if let Err(error) = ensure_session_workspace(&session, workspace) {
            return failed(
                error,
                RunProgress::new(initial),
                Some(session.metadata().clone()),
            );
        }
        if let Err(error) = repository.append(&mut session, user) {
            return failed(
                error,
                RunProgress::new(initial),
                Some(session.metadata().clone()),
            );
        }
        let mut messages = self.system_messages();
        messages.extend(session.chat_messages());
        let execution = self
            .turn_executor
            .run_outcome(self.completion_request(messages), handler, context)
            .await;
        match execution {
            RunOutcome::Completed(result) => {
                let generated = &result.messages[result.input_messages..];
                if let Err(error) = repository.append_all(&mut session, generated.iter().cloned()) {
                    return failed(error, result.progress, Some(session.metadata().clone()));
                }
                AgentTurnOutcome::Completed(AgentTurnResult {
                    execution_result: result,
                    session: Some(session.metadata().clone()),
                })
            }
            stopped => from_execution(stopped, Some(session.metadata().clone())),
        }
    }
    fn system_messages(&self) -> Vec<Message> {
        self.config
            .system_prompt
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(|p| Message::text(MessageRole::System, p.clone()))
            .into_iter()
            .collect()
    }
    fn completion_request(&self, messages: Vec<Message>) -> ModelRequest {
        let mut request = ModelRequest::new(self.config.model.clone(), messages);
        request.options = self.config.options.clone();
        request
    }
}
fn from_execution(outcome: RunOutcome, session: Option<SessionMetadata>) -> AgentTurnOutcome {
    match outcome {
        RunOutcome::Completed(execution_result) => AgentTurnOutcome::Completed(AgentTurnResult {
            execution_result,
            session,
        }),
        RunOutcome::Interrupted(turn) => interrupted(turn.stop, turn.progress, session),
    }
}
fn interrupted(
    stop: RunStop,
    progress: RunProgress,
    session: Option<SessionMetadata>,
) -> AgentTurnOutcome {
    let turn = InterruptedAgentTurn {
        session,
        progress,
        error: stop.error(),
        stop,
    };
    match &turn.stop {
        RunStop::Cancelled { .. } => AgentTurnOutcome::Aborted(turn),
        RunStop::Budget(_) => AgentTurnOutcome::Limited(turn),
        RunStop::Model(_) => AgentTurnOutcome::Stopped(turn),
        RunStop::Failed(_) => AgentTurnOutcome::Failed(turn),
    }
}
fn failed(
    error: anyhow::Error,
    mut progress: RunProgress,
    session: Option<SessionMetadata>,
) -> AgentTurnOutcome {
    progress.stage = RunStage::Persistence;
    interrupted(
        RunStop::Failed(RunFailure::from_error(RunStage::Persistence, &error)),
        progress,
        session,
    )
}
fn ensure_session_workspace(session: &Session, workspace: &std::path::Path) -> Result<()> {
    ensure!(
        session.metadata().cwd == workspace,
        "Session {} belongs to workspace {}, not {}",
        session.metadata().id,
        session.metadata().cwd.display(),
        workspace.display()
    );
    Ok(())
}
fn select_session<R: SessionRepository>(
    repository: &R,
    workspace: &std::path::Path,
    selection: SessionSelection,
) -> Result<Session> {
    match selection {
        SessionSelection::New => repository.create(workspace),
        SessionSelection::ContinueMostRecent => match repository.most_recent(workspace)? {
            Some(recent) => repository.open(&recent.id),
            None => repository.create(workspace),
        },
        SessionSelection::Existing(id) => repository.open(&id),
    }
}
#[cfg(test)]
mod tests {
    use crate::TurnCancelled;
    use anyhow::Context;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        ModelProvider, model::ModelResponse, session::MemorySessionRepository, tools::ToolRegistry,
    };

    struct RecordingProvider {
        responses: Mutex<VecDeque<ModelResponse>>,
        requests: Mutex<Vec<ModelRequest>>,
    }

    struct PendingProvider {
        started: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl ModelProvider for PendingProvider {
        fn model_spec(
            &self,
            model: &str,
        ) -> std::result::Result<crate::model::ModelSpec, crate::model::ProviderError> {
            let mut spec = crate::model::ModelSpec::new("scripted", model);
            spec.capabilities = crate::model::ModelCapabilities {
                tools: true,
                images: true,
                reasoning_history: true,
                temperature: true,
                top_p: true,
                seed: true,
                stop_sequences: true,
                structured_output: true,
            };
            Ok(spec)
        }
        async fn generate(
            &self,
            _request: ModelRequest,
            events: Arc<dyn crate::model::ModelEventSink>,
            _context: crate::TurnExecutionContext,
        ) -> std::result::Result<ModelResponse, crate::model::ProviderError> {
            let result: anyhow::Result<ModelResponse> = async {
                self.started.notify_one();
                std::future::pending().await
            }
            .await;
            match result {
                Ok(response) => {
                    crate::model::emit_response(&response, events.as_ref());
                    Ok(response)
                }
                Err(error) => Err(crate::model::ProviderError::new(
                    "scripted",
                    crate::model::ProviderErrorKind::Other,
                    error.to_string(),
                )),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for RecordingProvider {
        fn model_spec(
            &self,
            model: &str,
        ) -> std::result::Result<crate::model::ModelSpec, crate::model::ProviderError> {
            let mut spec = crate::model::ModelSpec::new("scripted", model);
            spec.capabilities = crate::model::ModelCapabilities {
                tools: true,
                images: true,
                reasoning_history: true,
                temperature: true,
                top_p: true,
                seed: true,
                stop_sequences: true,
                structured_output: true,
            };
            Ok(spec)
        }
        async fn generate(
            &self,
            request: ModelRequest,
            events: Arc<dyn crate::model::ModelEventSink>,
            _context: crate::TurnExecutionContext,
        ) -> std::result::Result<ModelResponse, crate::model::ProviderError> {
            let result: anyhow::Result<ModelResponse> = async {
                self.requests.lock().unwrap().push(request);
                self.responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .context("No turn runner test response")
            }
            .await;
            match result {
                Ok(response) => {
                    crate::model::emit_response(&response, events.as_ref());
                    Ok(response)
                }
                Err(error) => Err(crate::model::ProviderError::new(
                    "scripted",
                    crate::model::ProviderErrorKind::Other,
                    error.to_string(),
                )),
            }
        }
    }

    fn response(content: &str) -> ModelResponse {
        serde_json::from_value(json!({"id":"response","model":"test-model","message":{"role":"assistant","content":[{"type":"text","text":content}]},"stop_reason":{"reason":"complete"},"usage":{"input_tokens":null,"output_tokens":null,"total_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null}}))
        .unwrap()
    }

    fn agent(
        workspace: &std::path::Path,
        responses: Vec<ModelResponse>,
    ) -> (Agent, Arc<RecordingProvider>) {
        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        });
        let turn_executor = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(workspace)
            .registry(ToolRegistry::default())
            .build()
            .unwrap();
        let config = AgentConfig::new("test-model").system_prompt("system context");
        (Agent::new(turn_executor, config), provider)
    }

    #[tokio::test]
    async fn agent_persists_and_reopens_conversation_sessions() {
        let workspace = tempdir().unwrap();
        let repository = MemorySessionRepository::new();
        let (agent, provider) = agent(
            workspace.path(),
            vec![response("first answer"), response("second answer")],
        );

        let first = agent
            .run_with_session(
                &repository,
                SessionSelection::New,
                "first question",
                Arc::new(()),
            )
            .await
            .unwrap();
        let session_id = first.session.unwrap().id;
        let second = agent
            .run_with_session(
                &repository,
                SessionSelection::Existing(session_id.clone()),
                "second question",
                Arc::new(()),
            )
            .await
            .unwrap();

        assert_eq!(second.session.unwrap().id, session_id);
        let stored = repository.open(&session_id).unwrap();
        assert_eq!(stored.messages().len(), 4);
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests[0].messages.len(), 2);
        assert_eq!(requests[1].messages.len(), 4);
        assert_eq!(requests[1].messages[0].role, MessageRole::System);
        assert_eq!(
            requests[1].messages[3].text_content().as_str(),
            "second question"
        );
    }

    #[tokio::test]
    async fn stateless_turn_does_not_create_a_session() {
        let workspace = tempdir().unwrap();
        let (agent, _) = agent(workspace.path(), vec![response("answer")]);

        let result = agent.run("question", Arc::new(())).await.unwrap();

        assert!(result.session.is_none());
        assert_eq!(
            result
                .execution_result
                .final_message
                .text_content()
                .as_str(),
            "answer"
        );
    }

    #[tokio::test]
    async fn pre_cancelled_session_turn_does_not_create_a_session() {
        let workspace = tempdir().unwrap();
        let repository = MemorySessionRepository::new();
        let (agent, _) = agent(workspace.path(), vec![response("unused")]);
        let context = TurnExecutionContext::new();
        context.cancel("cancel before model request");

        let error = agent
            .run_with_session_context(
                &repository,
                SessionSelection::New,
                "question",
                Arc::new(()),
                context,
            )
            .await
            .unwrap_err();

        assert!(error.downcast_ref::<crate::TurnCancelled>().is_some());
        let sessions = repository.list(workspace.path()).unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn inflight_cancelled_session_turn_keeps_only_the_user_prompt() {
        let workspace = tempdir().unwrap();
        let repository = Arc::new(MemorySessionRepository::new());
        let started = Arc::new(tokio::sync::Notify::new());
        let executor = TurnExecutor::builder(PendingProvider {
            started: Arc::clone(&started),
        })
        .workspace(workspace.path())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
        let agent = Agent::new(executor, AgentConfig::new("test-model"));
        let context = TurnExecutionContext::new();
        let cancellation = context.clone();
        let repository_for_run = Arc::clone(&repository);

        let run = tokio::spawn(async move {
            agent
                .run_with_session_context(
                    repository_for_run.as_ref(),
                    SessionSelection::New,
                    "question",
                    Arc::new(()),
                    context,
                )
                .await
        });
        started.notified().await;
        cancellation.cancel("stop inflight turn");
        let error = run.await.unwrap().unwrap_err();

        assert!(error.downcast_ref::<crate::TurnCancelled>().is_some());
        let sessions = repository.list(workspace.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        let stored = repository.open(&sessions[0].id).unwrap();
        assert_eq!(stored.messages().len(), 1);
        assert_eq!(stored.messages()[0].message.role, MessageRole::User);
    }

    #[tokio::test]
    async fn structured_outcome_keeps_cancelled_session_identity_and_progress() {
        let workspace = tempdir().unwrap();
        let repository = Arc::new(MemorySessionRepository::new());
        let started = Arc::new(tokio::sync::Notify::new());
        let executor = TurnExecutor::builder(PendingProvider {
            started: Arc::clone(&started),
        })
        .workspace(workspace.path())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
        let agent = Agent::new(executor, AgentConfig::new("test-model"));
        let context = TurnExecutionContext::new();
        let cancellation = context.clone();
        let repository_for_run = Arc::clone(&repository);

        let run = tokio::spawn(async move {
            agent
                .run_with_session_outcome(
                    repository_for_run.as_ref(),
                    SessionSelection::New,
                    "question",
                    Arc::new(()),
                    context,
                )
                .await
        });
        started.notified().await;
        cancellation.cancel("stop from tui");

        let outcome = run.await.unwrap();
        let AgentTurnOutcome::Aborted(interrupted) = outcome else {
            panic!("expected aborted outcome");
        };
        let session = interrupted
            .session
            .as_ref()
            .expect("cancelled session metadata");
        assert_eq!(session.title.as_deref(), Some("question"));
        assert_eq!(interrupted.rounds, 1);
        assert!(interrupted.error.downcast_ref::<TurnCancelled>().is_some());
        assert_eq!(repository.open(&session.id).unwrap().messages().len(), 1);
    }
}
