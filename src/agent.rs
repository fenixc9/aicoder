//! High-level agent API for stateless or session-backed user turns.

use std::sync::Arc;

use anyhow::{Context, Result, ensure};

use crate::{
    AgentEventHandler, TurnExecutionResult, TurnExecutor,
    session::{Session, SessionMetadata, SessionRepository},
    types::{ChatCompletionRequest, ChatMessage, ResponseType, Role},
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
        let mut messages = self.system_messages();
        messages.push(user_message(prompt.into()));
        let execution_result = self
            .turn_executor
            .run_with_handler(self.completion_request(messages), handler)
            .await?;
        Ok(AgentTurnResult {
            execution_result,
            session: None,
        })
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
            .run_with_handler(self.completion_request(messages), handler)
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
}
