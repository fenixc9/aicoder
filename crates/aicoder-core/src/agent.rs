//! High-level agent API for stateless or session-backed user turns.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};

use crate::{
    AgentEventHandler, AgentRawEvent, AgentRawEventEnvelope, TurnCancelled, TurnExecutionContext,
    TurnExecutionResult, TurnExecutor,
    events::dispatch_event,
    session::{Session, SessionMetadata, SessionRepository},
    types::{ChatCompletionRequest, ChatMessage, ResponseType, Role, Usage},
};

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<i32>,
    pub seed: Option<i32>,
    pub stop: Option<Vec<String>>,
    pub response_format: Option<ResponseType>,
}

impl AgentConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            seed: None,
            stop: None,
            response_format: None,
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
    pub usage: Usage,
    pub rounds: usize,
    pub error: anyhow::Error,
}

#[derive(Debug)]
pub enum AgentTurnOutcome {
    Completed(AgentTurnResult),
    Aborted(InterruptedAgentTurn),
    Failed(InterruptedAgentTurn),
}

impl AgentTurnOutcome {
    pub fn session(&self) -> Option<&SessionMetadata> {
        match self {
            Self::Completed(result) => result.session.as_ref(),
            Self::Aborted(turn) | Self::Failed(turn) => turn.session.as_ref(),
        }
    }

    pub fn into_result(self) -> Result<AgentTurnResult> {
        match self {
            Self::Completed(result) => Ok(result),
            Self::Aborted(turn) | Self::Failed(turn) => Err(turn.error),
        }
    }
}

#[derive(Default, Clone)]
struct TurnProgress {
    usage: Usage,
    rounds: usize,
}

struct ProgressTrackingHandler {
    inner: Arc<dyn AgentEventHandler>,
    progress: Arc<Mutex<TurnProgress>>,
}

impl AgentEventHandler for ProgressTrackingHandler {
    fn on_raw_event(&self, envelope: &AgentRawEventEnvelope) {
        let mut progress = self.progress.lock().expect("turn progress lock poisoned");
        progress.rounds = progress.rounds.max(envelope.round.unwrap_or(0));
        if let AgentRawEvent::UsageUpdated { usage } = &envelope.event {
            progress.usage.accumulate(usage);
        }
        drop(progress);
        dispatch_event(self.inner.as_ref(), envelope);
    }
}

/// Owns request construction and optional conversation persistence around a `TurnExecutor`.
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
        let mut messages = self.system_messages();
        messages.push(user_message(prompt.into()));
        let execution_result = self
            .turn_executor
            .run_with_context(self.completion_request(messages), handler, context)
            .await?;
        Ok(AgentTurnResult {
            execution_result,
            session: None,
        })
    }

    pub async fn run_outcome(
        &self,
        prompt: impl Into<String>,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
    ) -> AgentTurnOutcome {
        let progress = Arc::new(Mutex::new(TurnProgress::default()));
        let tracking = Arc::new(ProgressTrackingHandler {
            inner: handler,
            progress: Arc::clone(&progress),
        });
        let result = self.run_with_context(prompt, tracking, context).await;
        classify_turn_result(result, None, progress)
    }

    pub async fn run_with_session<R>(
        &self,
        repository: &R,
        selection: SessionSelection,
        prompt: impl Into<String>,
        handler: Arc<dyn AgentEventHandler>,
    ) -> Result<AgentTurnResult>
    where
        R: SessionRepository,
    {
        self.run_with_session_context(
            repository,
            selection,
            prompt,
            handler,
            TurnExecutionContext::new(),
        )
        .await
    }

    pub async fn run_with_session_context<R>(
        &self,
        repository: &R,
        selection: SessionSelection,
        prompt: impl Into<String>,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
    ) -> Result<AgentTurnResult>
    where
        R: SessionRepository,
    {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let workspace = self.turn_executor.workspace_root();
        let mut session = select_session(repository, workspace, selection)?;
        ensure!(
            session.metadata().cwd == workspace,
            "Session {} belongs to workspace {}, not {}",
            session.metadata().id,
            session.metadata().cwd.display(),
            workspace.display()
        );

        repository.append(&mut session, user_message(prompt.into()))?;
        let mut messages = self.system_messages();
        messages.extend(session.chat_messages());
        let input_message_count = messages.len();
        let execution_result = self
            .turn_executor
            .run_with_context(self.completion_request(messages), handler, context)
            .await?;
        let generated_messages = execution_result
            .messages
            .get(input_message_count..)
            .context("TurnExecutor returned fewer messages than supplied conversation context")?;
        repository.append_all(&mut session, generated_messages.iter().cloned())?;
        Ok(AgentTurnResult {
            execution_result,
            session: Some(session.metadata().clone()),
        })
    }

    pub async fn run_with_session_outcome<R>(
        &self,
        repository: &R,
        selection: SessionSelection,
        prompt: impl Into<String>,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
    ) -> AgentTurnOutcome
    where
        R: SessionRepository,
    {
        if context.is_cancelled() {
            return interrupted_outcome(context.error().into(), None, TurnProgress::default());
        }
        let workspace = self.turn_executor.workspace_root();
        let mut session = match select_session(repository, workspace, selection) {
            Ok(session) => session,
            Err(error) => {
                return AgentTurnOutcome::Failed(InterruptedAgentTurn {
                    session: None,
                    usage: Usage::default(),
                    rounds: 0,
                    error,
                });
            }
        };
        if let Err(error) = ensure_session_workspace(&session, workspace) {
            return interrupted_outcome(
                error,
                Some(session.metadata().clone()),
                TurnProgress::default(),
            );
        }
        if let Err(error) = repository.append(&mut session, user_message(prompt.into())) {
            return interrupted_outcome(
                error,
                Some(session.metadata().clone()),
                TurnProgress::default(),
            );
        }
        let metadata = session.metadata().clone();

        let mut messages = self.system_messages();
        messages.extend(session.chat_messages());
        let input_message_count = messages.len();
        let progress = Arc::new(Mutex::new(TurnProgress::default()));
        let tracking = Arc::new(ProgressTrackingHandler {
            inner: handler,
            progress: Arc::clone(&progress),
        });
        let execution = self
            .turn_executor
            .run_with_context(self.completion_request(messages), tracking, context)
            .await;
        let execution_result = match execution {
            Ok(result) => result,
            Err(error) => return classify_turn_result(Err(error), Some(metadata), progress),
        };
        let generated_messages = match execution_result.messages.get(input_message_count..) {
            Some(messages) => messages,
            None => {
                return interrupted_outcome(
                    anyhow::anyhow!(
                        "TurnExecutor returned fewer messages than supplied conversation context"
                    ),
                    Some(metadata),
                    progress
                        .lock()
                        .expect("turn progress lock poisoned")
                        .clone(),
                );
            }
        };
        if let Err(error) = repository.append_all(&mut session, generated_messages.iter().cloned())
        {
            return interrupted_outcome(
                error,
                Some(session.metadata().clone()),
                progress
                    .lock()
                    .expect("turn progress lock poisoned")
                    .clone(),
            );
        }
        AgentTurnOutcome::Completed(AgentTurnResult {
            execution_result,
            session: Some(session.metadata().clone()),
        })
    }

    fn system_messages(&self) -> Vec<ChatMessage> {
        self.config
            .system_prompt
            .as_ref()
            .filter(|prompt| !prompt.is_empty())
            .map(|prompt| ChatMessage {
                role: Role::System,
                content: Some(prompt.clone()),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            })
            .into_iter()
            .collect()
    }

    fn completion_request(&self, messages: Vec<ChatMessage>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature: self.config.temperature,
            top_p: self.config.top_p,
            max_tokens: self.config.max_tokens,
            seed: self.config.seed,
            tools: None,
            tool_choice: None,
            stream: None,
            stream_options: None,
            stop: self.config.stop.clone(),
            response_format: self.config.response_format.clone(),
        }
    }
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

fn classify_turn_result(
    result: Result<AgentTurnResult>,
    session: Option<SessionMetadata>,
    progress: Arc<Mutex<TurnProgress>>,
) -> AgentTurnOutcome {
    match result {
        Ok(result) => AgentTurnOutcome::Completed(result),
        Err(error) => {
            let progress = progress
                .lock()
                .expect("turn progress lock poisoned")
                .clone();
            interrupted_outcome(error, session, progress)
        }
    }
}

fn interrupted_outcome(
    error: anyhow::Error,
    session: Option<SessionMetadata>,
    progress: TurnProgress,
) -> AgentTurnOutcome {
    let turn = InterruptedAgentTurn {
        session,
        usage: progress.usage,
        rounds: progress.rounds,
        error,
    };
    if turn.error.downcast_ref::<TurnCancelled>().is_some() {
        AgentTurnOutcome::Aborted(turn)
    } else {
        AgentTurnOutcome::Failed(turn)
    }
}

fn select_session<R>(
    repository: &R,
    workspace: &std::path::Path,
    selection: SessionSelection,
) -> Result<Session>
where
    R: SessionRepository,
{
    match selection {
        SessionSelection::New => repository.create(workspace),
        SessionSelection::ContinueMostRecent => match repository.most_recent(workspace)? {
            Some(recent) => repository.open(&recent.id),
            None => repository.create(workspace),
        },
        SessionSelection::Existing(id) => repository.open(&id),
    }
}

fn user_message(content: String) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: Some(content),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        ChatCompletionProvider, session::MemorySessionRepository, tools::ToolRegistry,
        types::ChatCompletionResponse,
    };

    struct RecordingProvider {
        responses: Mutex<VecDeque<ChatCompletionResponse>>,
        requests: Mutex<Vec<ChatCompletionRequest>>,
    }

    struct PendingProvider {
        started: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl ChatCompletionProvider for PendingProvider {
        async fn complete(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse> {
            self.started.notify_one();
            std::future::pending().await
        }
    }

    #[async_trait]
    impl ChatCompletionProvider for RecordingProvider {
        async fn complete(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .context("No turn runner test response")
        }
    }

    fn response(content: &str) -> ChatCompletionResponse {
        serde_json::from_value(json!({
            "id": "response",
            "object": "chat.completion",
            "created": 1,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop"
            }]
        }))
        .unwrap()
    }

    fn agent(
        workspace: &std::path::Path,
        responses: Vec<ChatCompletionResponse>,
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
        assert_eq!(requests[1].messages[0].role, Role::System);
        assert_eq!(
            requests[1].messages[3].content.as_deref(),
            Some("second question")
        );
    }

    #[tokio::test]
    async fn stateless_turn_does_not_create_a_session() {
        let workspace = tempdir().unwrap();
        let (agent, _) = agent(workspace.path(), vec![response("answer")]);

        let result = agent.run("question", Arc::new(())).await.unwrap();

        assert!(result.session.is_none());
        assert_eq!(
            result.execution_result.final_message.content.as_deref(),
            Some("answer")
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
        assert_eq!(stored.messages()[0].message.role, Role::User);
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
        let session = interrupted.session.expect("cancelled session metadata");
        assert_eq!(session.title.as_deref(), Some("question"));
        assert_eq!(interrupted.rounds, 1);
        assert!(interrupted.error.downcast_ref::<TurnCancelled>().is_some());
        assert_eq!(repository.open(&session.id).unwrap().messages().len(), 1);
    }
}
