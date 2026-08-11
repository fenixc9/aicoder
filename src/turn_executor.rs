//! Executes one agent turn through its model, tool, and completion-verification rounds.

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::{
    client::ChatClient,
    completion::{
        AcceptAllCompletionVerifier, CompletionContext, CompletionVerdict, CompletionVerifier,
    },
    events::{
        AgentEventEmitter, AgentEventHandler, AgentRawEvent, AgentStage, RoundOutcome,
        emit_full_response_events,
    },
    state::{AgentRunState, AgentRunStateMachine},
    tools::{
        ApprovalHandler, DenyAllApproval, DispatcherConfig, ToolDispatcher, ToolRegistry,
        default_registry,
    },
    types::{
        ChatCompletionRequest, ChatCompletionResponse, ChatMessage, FinishReason, ToolChoice,
        ToolChoiceMode, Usage,
    },
};

#[async_trait]
pub trait ChatCompletionProvider: Send + Sync {
    async fn complete(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse>;

    async fn complete_with_events(
        &self,
        request: ChatCompletionRequest,
        events: AgentEventEmitter,
    ) -> Result<ChatCompletionResponse> {
        let response = self.complete(request).await?;
        emit_full_response_events(&events, &response);
        Ok(response)
    }
}

#[async_trait]
impl ChatCompletionProvider for ChatClient {
    async fn complete(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        if request.stream.unwrap_or(false) {
            self.chat_completion_stream_collect(request).await
        } else {
            self.chat_completion(request).await
        }
    }

    async fn complete_with_events(
        &self,
        request: ChatCompletionRequest,
        events: AgentEventEmitter,
    ) -> Result<ChatCompletionResponse> {
        if request.stream.unwrap_or(false) {
            self.chat_completion_stream_collect_with_events(request, &events)
                .await
        } else {
            let response = self.chat_completion_with_events(request, &events).await?;
            emit_full_response_events(&events, &response);
            Ok(response)
        }
    }
}

#[derive(Debug, Clone)]
pub struct TurnExecutionConfig {
    pub max_rounds: usize,
    pub stream: bool,
}

impl Default for TurnExecutionConfig {
    fn default() -> Self {
        Self {
            max_rounds: 8,
            stream: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TurnExecutionResult {
    pub final_message: ChatMessage,
    pub finish_reason: Option<FinishReason>,
    pub messages: Vec<ChatMessage>,
    pub usage: Usage,
    pub rounds: usize,
}

pub struct TurnExecutor {
    provider: Arc<dyn ChatCompletionProvider>,
    dispatcher: Arc<ToolDispatcher>,
    completion_verifier: Arc<dyn CompletionVerifier>,
    config: TurnExecutionConfig,
}

impl TurnExecutor {
    pub fn builder<P>(provider: P) -> TurnExecutorBuilder
    where
        P: ChatCompletionProvider + 'static,
    {
        TurnExecutorBuilder::new(provider)
    }

    pub fn builder_from_shared(provider: Arc<dyn ChatCompletionProvider>) -> TurnExecutorBuilder {
        TurnExecutorBuilder::from_shared(provider)
    }

    pub fn new(
        provider: Arc<dyn ChatCompletionProvider>,
        dispatcher: Arc<ToolDispatcher>,
        config: TurnExecutionConfig,
    ) -> Self {
        Self::new_with_verifier(
            provider,
            dispatcher,
            Arc::new(AcceptAllCompletionVerifier),
            config,
        )
    }

    fn new_with_verifier(
        provider: Arc<dyn ChatCompletionProvider>,
        dispatcher: Arc<ToolDispatcher>,
        completion_verifier: Arc<dyn CompletionVerifier>,
        config: TurnExecutionConfig,
    ) -> Self {
        Self {
            provider,
            dispatcher,
            completion_verifier,
            config,
        }
    }

    pub fn workspace_root(&self) -> &std::path::Path {
        self.dispatcher.workspace_root()
    }

    pub async fn run(&self, request: ChatCompletionRequest) -> Result<TurnExecutionResult> {
        self.run_with_handler(request, Arc::new(())).await
    }

    pub async fn run_with_handler(
        &self,
        request: ChatCompletionRequest,
        handler: Arc<dyn AgentEventHandler>,
    ) -> Result<TurnExecutionResult> {
        let events = AgentEventEmitter::new(handler);
        events.emit(AgentRawEvent::AgentStarted {
            model: request.model.clone().into(),
        });
        let mut state = AgentRunStateMachine::new();
        let execution = match transition_state(&mut state, &events, AgentRunState::Preparing) {
            Ok(()) => self.run_inner(request, &events, &mut state).await,
            Err(error) => Err(error),
        };
        let result = match execution {
            Ok(run) => transition_state(
                &mut state,
                &events,
                AgentRunState::Completed { rounds: run.rounds },
            )
            .map(|()| run),
            Err(error) => {
                let failed = AgentRunState::Failed {
                    round: state.state().round(),
                };
                match transition_state(&mut state, &events, failed) {
                    Ok(()) => Err(error),
                    Err(state_error) => Err(error.context(format!(
                        "Turn execution state machine also failed while terminating: {state_error}"
                    ))),
                }
            }
        };
        match &result {
            Ok(result) => events.emit(AgentRawEvent::AgentCompleted {
                rounds: result.rounds,
                usage: result.usage.clone().into(),
            }),
            Err(error) => events.emit(AgentRawEvent::AgentFailed {
                stage: AgentStage::Turn,
                message: format!("{error:#}").into(),
            }),
        }
        events.shutdown().await;
        result
    }

    async fn run_inner(
        &self,
        mut request: ChatCompletionRequest,
        events: &AgentEventEmitter,
        state: &mut AgentRunStateMachine,
    ) -> Result<TurnExecutionResult> {
        if self.config.max_rounds == 0 {
            anyhow::bail!("Turn execution max_rounds must be greater than zero");
        }

        request.stream = Some(self.config.stream);
        let definitions = self.dispatcher.registry().definitions();
        if !definitions.is_empty() {
            request.tools = Some(definitions);
            request.tool_choice = Some(ToolChoice::Mode(ToolChoiceMode::Auto));
        }

        let mut messages = request.messages.clone();
        let mut usage = Usage::default();

        for round in 1..=self.config.max_rounds {
            let round_events = events.for_round(round);
            round_events.emit(AgentRawEvent::RoundStarted);
            transition_state(state, &round_events, AgentRunState::AwaitingModel { round })?;
            request.messages = messages.clone();
            round_events.emit(AgentRawEvent::ModelRequestStarted);
            let response = match self
                .provider
                .complete_with_events(request.clone(), round_events.clone())
                .await
                .with_context(|| format!("Model request failed in agent round {round}"))
            {
                Ok(response) => response,
                Err(error) => {
                    round_events.emit(AgentRawEvent::ModelResponseFailed {
                        message: format!("{error:#}").into(),
                    });
                    round_events.emit(AgentRawEvent::RoundCompleted {
                        outcome: RoundOutcome::Failed,
                    });
                    return Err(error);
                }
            };
            if let Some(round_usage) = response.usage {
                usage.accumulate(&round_usage);
            }

            let choice = response
                .choices
                .into_iter()
                .next()
                .context("Model response contains no choices")?;
            let finish_reason = choice.finish_reason;
            let assistant_message = choice.message;
            let tool_calls = assistant_message.tool_calls.clone().unwrap_or_default();
            messages.push(assistant_message.clone());

            if tool_calls.is_empty() {
                transition_state(
                    state,
                    &round_events,
                    AgentRunState::VerifyingCompletion { round },
                )?;
                round_events.emit(AgentRawEvent::CompletionVerificationStarted);
                let verdict = self
                    .completion_verifier
                    .verify(CompletionContext {
                        workspace: self.workspace_root(),
                        round,
                        candidate: &assistant_message,
                        messages: &messages,
                        usage: &usage,
                    })
                    .await
                    .with_context(|| {
                        format!("Completion verification failed in agent round {round}")
                    });
                match verdict {
                    Ok(CompletionVerdict::Accepted) => {
                        round_events.emit(AgentRawEvent::CompletionVerificationEnded {
                            outcome: crate::events::CompletionVerificationOutcome::Accepted,
                            feedback: None,
                        });
                        round_events.emit(AgentRawEvent::RoundCompleted {
                            outcome: RoundOutcome::FinalAnswer,
                        });
                        return Ok(TurnExecutionResult {
                            final_message: assistant_message,
                            finish_reason,
                            messages,
                            usage,
                            rounds: round,
                        });
                    }
                    Ok(CompletionVerdict::Rejected { feedback }) => {
                        let feedback = bounded_verifier_feedback(feedback);
                        round_events.emit(AgentRawEvent::CompletionVerificationEnded {
                            outcome: crate::events::CompletionVerificationOutcome::Rejected,
                            feedback: Some(feedback.clone().into()),
                        });
                        messages.push(completion_feedback_message(feedback));
                        round_events.emit(AgentRawEvent::RoundCompleted {
                            outcome: RoundOutcome::CompletionRejected,
                        });
                        continue;
                    }
                    Err(error) => {
                        round_events.emit(AgentRawEvent::CompletionVerificationEnded {
                            outcome: crate::events::CompletionVerificationOutcome::Failed,
                            feedback: Some(format!("{error:#}").into()),
                        });
                        round_events.emit(AgentRawEvent::RoundCompleted {
                            outcome: RoundOutcome::Failed,
                        });
                        return Err(error);
                    }
                }
            }

            transition_state(
                state,
                &round_events,
                AgentRunState::ExecutingTools {
                    round,
                    count: tool_calls.len(),
                },
            )?;
            tracing::debug!(
                round,
                calls = tool_calls.len(),
                "Dispatching model tool calls"
            );
            let tool_messages = match self
                .dispatcher
                .dispatch_with_events(&tool_calls, &round_events)
                .await
            {
                Ok(messages) => messages,
                Err(error) => {
                    round_events.emit(AgentRawEvent::RoundCompleted {
                        outcome: RoundOutcome::Failed,
                    });
                    return Err(error);
                }
            };
            messages.extend(tool_messages);
            round_events.emit(AgentRawEvent::RoundCompleted {
                outcome: RoundOutcome::ToolCalls {
                    count: tool_calls.len(),
                },
            });
        }

        anyhow::bail!(
            "Turn execution exceeded maximum of {} model rounds",
            self.config.max_rounds
        )
    }
}

fn transition_state(
    state: &mut AgentRunStateMachine,
    events: &AgentEventEmitter,
    next: AgentRunState,
) -> Result<()> {
    let transition = state.transition(next)?;
    events.emit(AgentRawEvent::StateChanged { transition });
    Ok(())
}

/// Convenience assembly for applications that want the built-in coding tools.
pub struct TurnExecutorBuilder {
    provider: Arc<dyn ChatCompletionProvider>,
    registry: Option<Arc<ToolRegistry>>,
    workspace: PathBuf,
    approval: Arc<dyn ApprovalHandler>,
    completion_verifier: Arc<dyn CompletionVerifier>,
    dispatcher_config: DispatcherConfig,
    execution_config: TurnExecutionConfig,
}

impl TurnExecutorBuilder {
    pub fn new<P>(provider: P) -> Self
    where
        P: ChatCompletionProvider + 'static,
    {
        Self::from_shared(Arc::new(provider))
    }

    pub fn from_shared(provider: Arc<dyn ChatCompletionProvider>) -> Self {
        Self {
            provider,
            registry: None,
            workspace: PathBuf::from("."),
            approval: Arc::new(DenyAllApproval),
            completion_verifier: Arc::new(AcceptAllCompletionVerifier),
            dispatcher_config: DispatcherConfig::default(),
            execution_config: TurnExecutionConfig::default(),
        }
    }

    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = workspace.into();
        self
    }

    pub fn registry(mut self, registry: ToolRegistry) -> Self {
        self.registry = Some(Arc::new(registry));
        self
    }

    pub fn shared_registry(mut self, registry: Arc<ToolRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn approval<A>(mut self, approval: A) -> Self
    where
        A: ApprovalHandler + 'static,
    {
        self.approval = Arc::new(approval);
        self
    }

    pub fn shared_approval(mut self, approval: Arc<dyn ApprovalHandler>) -> Self {
        self.approval = approval;
        self
    }

    pub fn completion_verifier<V>(mut self, verifier: V) -> Self
    where
        V: CompletionVerifier + 'static,
    {
        self.completion_verifier = Arc::new(verifier);
        self
    }

    pub fn shared_completion_verifier(mut self, verifier: Arc<dyn CompletionVerifier>) -> Self {
        self.completion_verifier = verifier;
        self
    }

    pub fn dispatcher_config(mut self, config: DispatcherConfig) -> Self {
        self.dispatcher_config = config;
        self
    }

    pub fn config(mut self, config: TurnExecutionConfig) -> Self {
        self.execution_config = config;
        self
    }

    pub fn build(self) -> Result<TurnExecutor> {
        let registry = match self.registry {
            Some(registry) => registry,
            None => Arc::new(default_registry()?),
        };
        let dispatcher = ToolDispatcher::new(
            registry,
            self.workspace,
            self.approval,
            self.dispatcher_config,
        )?;
        Ok(TurnExecutor::new_with_verifier(
            self.provider,
            Arc::new(dispatcher),
            self.completion_verifier,
            self.execution_config,
        ))
    }
}

const MAX_VERIFIER_FEEDBACK_BYTES: usize = 16 * 1024;

fn bounded_verifier_feedback(mut feedback: String) -> String {
    if feedback.len() <= MAX_VERIFIER_FEEDBACK_BYTES {
        return feedback;
    }
    let mut boundary = MAX_VERIFIER_FEEDBACK_BYTES;
    while boundary > 0 && !feedback.is_char_boundary(boundary) {
        boundary -= 1;
    }
    feedback.truncate(boundary);
    feedback.push_str("\n[verification feedback truncated]");
    feedback
}

fn completion_feedback_message(feedback: String) -> ChatMessage {
    ChatMessage {
        // Keep the verifier observation in session history without treating it as a new
        // application-owned system prompt.
        role: crate::types::Role::User,
        content: Some(format!(
            "Automated completion verification rejected the previous answer. Continue working and address this observation before answering again.\n\n{feedback}"
        )),
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
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::*;
    use crate::{
        tools::{
            AllowAllApproval, DispatcherConfig, ExecutableTool, ToolCapability, ToolContext,
            ToolFailure, ToolRegistry, ToolSuccess,
        },
        types::FunctionDefinition,
    };

    struct MockProvider {
        responses: Mutex<VecDeque<ChatCompletionResponse>>,
        requests: Mutex<Vec<ChatCompletionRequest>>,
    }

    impl MockProvider {
        fn new(responses: Vec<ChatCompletionResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ChatCompletionProvider for MockProvider {
        async fn complete(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .context("No mock response available")
        }
    }

    struct EchoTool;

    struct RejectOnceVerifier {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl CompletionVerifier for RejectOnceVerifier {
        async fn verify(&self, _context: CompletionContext<'_>) -> Result<CompletionVerdict> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(CompletionVerdict::rejected("tests have not passed"))
            } else {
                Ok(CompletionVerdict::Accepted)
            }
        }
    }

    #[derive(Default)]
    struct RecordingHandler(Mutex<Vec<AgentRawEvent>>);

    impl AgentEventHandler for RecordingHandler {
        fn on_raw_event(&self, event: &crate::events::AgentRawEventEnvelope) {
            self.0.lock().unwrap().push(event.event.clone());
        }
    }

    #[async_trait]
    impl ExecutableTool for EchoTool {
        fn definition(&self) -> FunctionDefinition {
            FunctionDefinition {
                name: "echo".to_string(),
                description: Some("Echo a value".to_string()),
                parameters: Some(json!({"type":"object"})),
            }
        }

        fn capability(&self) -> ToolCapability {
            ToolCapability::ReadOnly
        }

        async fn execute(
            &self,
            _context: &ToolContext,
            arguments: Value,
        ) -> std::result::Result<ToolSuccess, ToolFailure> {
            Ok(ToolSuccess {
                output: arguments,
                truncated: false,
            })
        }
    }

    fn response(value: Value) -> ChatCompletionResponse {
        serde_json::from_value(value).unwrap()
    }

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "deepseek-chat".to_string(),
            messages: vec![ChatMessage {
                role: crate::types::Role::User,
                content: Some("echo hello".to_string()),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            temperature: None,
            top_p: None,
            max_tokens: Some(128),
            seed: None,
            tools: None,
            tool_choice: None,
            stream: Some(true),
            stream_options: None,
            stop: None,
            response_format: None,
        }
    }

    #[tokio::test]
    async fn agent_executes_tool_and_returns_final_answer() {
        let provider = Arc::new(MockProvider::new(vec![
            response(json!({
                "id":"one","object":"chat.completion","created":1,"model":"deepseek-chat",
                "choices":[{"index":0,"message":{"role":"assistant","content":null,
                    "tool_calls":[{"id":"call-1","type":"function","function":{"name":"echo","arguments":"{\"value\":\"hello\"}"}}]},
                    "finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12,
                    "prompt_tokens_details":{"cached_tokens":4,"cache_write_tokens":2}}
            })),
            response(json!({
                "id":"two","object":"chat.completion","created":2,"model":"deepseek-chat",
                "choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":15,"completion_tokens":1,"total_tokens":16,
                    "prompt_tokens_details":{"cached_tokens":10}}
            })),
        ]));
        let directory = tempdir().unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).unwrap();
        let dispatcher = Arc::new(
            ToolDispatcher::new(
                Arc::new(registry),
                directory.path(),
                Arc::new(AllowAllApproval),
                DispatcherConfig::default(),
            )
            .unwrap(),
        );
        let agent = TurnExecutor::new(provider.clone(), dispatcher, TurnExecutionConfig::default());

        let result = agent.run(request()).await.unwrap();

        assert_eq!(result.final_message.content.as_deref(), Some("done"));
        assert_eq!(result.rounds, 2);
        assert_eq!(result.usage.total_tokens, 28);
        assert_eq!(result.usage.cached_tokens(), 14);
        assert_eq!(result.usage.uncached_tokens(), 11);
        assert_eq!(
            result
                .usage
                .prompt_tokens_details
                .as_ref()
                .unwrap()
                .cache_write_tokens,
            Some(2)
        );
        assert_eq!(result.messages.len(), 4);
        assert_eq!(result.messages[2].tool_call_id.as_deref(), Some("call-1"));

        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].stream, Some(true));
        assert_eq!(requests[0].tools.as_ref().unwrap().len(), 1);
        assert_eq!(requests[1].messages.len(), 3);
    }

    #[tokio::test]
    async fn agent_stops_at_round_limit() {
        let tool_response = || {
            response(json!({
                "id":"loop","object":"chat.completion","created":1,"model":"deepseek-chat",
                "choices":[{"index":0,"message":{"role":"assistant","content":null,
                    "tool_calls":[{"id":"call-loop","type":"function","function":{"name":"echo","arguments":"{}"}}]},
                    "finish_reason":"tool_calls"}]
            }))
        };
        let provider = Arc::new(MockProvider::new(vec![tool_response(), tool_response()]));
        let directory = tempdir().unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).unwrap();
        let dispatcher = Arc::new(
            ToolDispatcher::new(
                Arc::new(registry),
                directory.path(),
                Arc::new(AllowAllApproval),
                DispatcherConfig::default(),
            )
            .unwrap(),
        );
        let agent = TurnExecutor::new(
            provider,
            dispatcher,
            TurnExecutionConfig {
                max_rounds: 2,
                stream: true,
            },
        );

        let error = agent.run(request()).await.unwrap_err();
        assert!(error.to_string().contains("exceeded maximum"));
    }

    #[tokio::test]
    async fn agent_feeds_tool_error_back_to_model() {
        let provider = Arc::new(MockProvider::new(vec![
            response(json!({
                "id":"one","object":"chat.completion","created":1,"model":"deepseek-chat",
                "choices":[{"index":0,"message":{"role":"assistant","content":null,
                    "tool_calls":[{"id":"bad-call","type":"function","function":{"name":"missing","arguments":"{}"}}]},
                    "finish_reason":"tool_calls"}]
            })),
            response(json!({
                "id":"two","object":"chat.completion","created":2,"model":"deepseek-chat",
                "choices":[{"index":0,"message":{"role":"assistant","content":"recovered"},"finish_reason":"stop"}]
            })),
        ]));
        let directory = tempdir().unwrap();
        let dispatcher = Arc::new(
            ToolDispatcher::new(
                Arc::new(ToolRegistry::default()),
                directory.path(),
                Arc::new(AllowAllApproval),
                DispatcherConfig::default(),
            )
            .unwrap(),
        );
        let agent = TurnExecutor::new(provider.clone(), dispatcher, TurnExecutionConfig::default());

        let result = agent.run(request()).await.unwrap();

        assert_eq!(result.final_message.content.as_deref(), Some("recovered"));
        let requests = provider.requests.lock().unwrap();
        let tool_result: Value =
            serde_json::from_str(requests[1].messages[2].content.as_ref().unwrap()).unwrap();
        assert_eq!(tool_result["error"]["code"], "unknown_tool");
    }

    #[tokio::test]
    async fn agent_continues_after_completion_rejection() {
        let provider = Arc::new(MockProvider::new(vec![
            response(json!({
                "id":"one","object":"chat.completion","created":1,"model":"deepseek-chat",
                "choices":[{"index":0,"message":{"role":"assistant","content":"done too early"},"finish_reason":"stop"}]
            })),
            response(json!({
                "id":"two","object":"chat.completion","created":2,"model":"deepseek-chat",
                "choices":[{"index":0,"message":{"role":"assistant","content":"verified"},"finish_reason":"stop"}]
            })),
        ]));
        let directory = tempdir().unwrap();
        let handler = Arc::new(RecordingHandler::default());
        let agent = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(directory.path())
            .approval(AllowAllApproval)
            .completion_verifier(RejectOnceVerifier {
                calls: AtomicUsize::new(0),
            })
            .build()
            .unwrap();

        let result = agent
            .run_with_handler(request(), handler.clone())
            .await
            .unwrap();

        assert_eq!(result.rounds, 2);
        assert_eq!(result.final_message.content.as_deref(), Some("verified"));
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].messages.len(), 3);
        assert_eq!(requests[1].messages[2].role, crate::types::Role::User);
        assert!(
            requests[1].messages[2]
                .content
                .as_deref()
                .unwrap()
                .contains("tests have not passed")
        );
        drop(requests);

        let events = handler.0.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::CompletionVerificationEnded {
                outcome: crate::events::CompletionVerificationOutcome::Rejected,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::CompletionVerificationEnded {
                outcome: crate::events::CompletionVerificationOutcome::Accepted,
                ..
            }
        )));
        let states = events
            .iter()
            .filter_map(|event| match event {
                AgentRawEvent::StateChanged { transition } => Some(transition.current),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            states,
            vec![
                AgentRunState::Preparing,
                AgentRunState::AwaitingModel { round: 1 },
                AgentRunState::VerifyingCompletion { round: 1 },
                AgentRunState::AwaitingModel { round: 2 },
                AgentRunState::VerifyingCompletion { round: 2 },
                AgentRunState::Completed { rounds: 2 },
            ]
        );
    }
}
