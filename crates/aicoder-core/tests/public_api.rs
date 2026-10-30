use aicoder_core::model::{ModelOptions, ModelToolChoice};
use std::sync::{Arc, Mutex};

use aicoder_core::{
    Agent, AgentConfig, AgentRawEvent, AgentRunState, AgentTurnOutcome, ContextWindowConfig,
    ModelProvider, PruningContextCompactor, SessionSelection, TurnCancelled, TurnExecutionContext,
    TurnExecutor,
    model::{Message, MessageRole, ModelRequest, ModelResponse},
    session::{MemorySessionRepository, SessionRepository},
    tools::ToolRegistry,
};
use async_trait::async_trait;
use tempfile::tempdir;

struct StaticProvider;

#[async_trait]
impl ModelProvider for StaticProvider {
    fn model_spec(
        &self,
        model: &str,
    ) -> std::result::Result<aicoder_core::model::ModelSpec, aicoder_core::model::ProviderError>
    {
        let mut spec = aicoder_core::model::ModelSpec::new("scripted", model);
        spec.capabilities = aicoder_core::model::ModelCapabilities {
            reasoning_efforts: Vec::new(),
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
        events: Arc<dyn aicoder_core::model::ModelEventSink>,
        _context: aicoder_core::TurnExecutionContext,
    ) -> std::result::Result<ModelResponse, aicoder_core::model::ProviderError> {
        assert!(
            request.tools.is_empty(),
            "core must not implicitly install IO tools"
        );
        let result: anyhow::Result<ModelResponse> = async {
        Ok(serde_json::from_value(serde_json::json!({"id":"response-1","model":request.model,"message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"stop_reason":{"reason":"complete"},"usage":{"input_tokens":null,"output_tokens":null,"total_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null}}))?)
    }.await;
        match result {
            Ok(response) => {
                aicoder_core::model::emit_response(&response, events.as_ref());
                Ok(response)
            }
            Err(error) => Err(aicoder_core::model::ProviderError::new(
                "scripted",
                aicoder_core::model::ProviderErrorKind::Other,
                error.to_string(),
            )),
        }
    }
}

#[tokio::test]
async fn external_application_can_build_turn_executor_and_observe_raw_events() {
    let workspace = tempdir().unwrap();
    let turn_executor = TurnExecutor::builder(StaticProvider)
        .workspace(workspace.path())
        .context_compactor(
            PruningContextCompactor::new(ContextWindowConfig {
                max_context_tokens: 32_768,
                reserved_output_tokens: 2_048,
                preserve_recent_tokens: 8_192,
            })
            .unwrap(),
        )
        .build()
        .unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let target = Arc::clone(&observed);
    let handler = Arc::new(move |event: &aicoder_core::events::AgentRawEventEnvelope| {
        target.lock().unwrap().push(event.event.clone());
    });
    let request = ModelRequest {
        model: "test-model".to_string(),
        messages: vec![Message::text(MessageRole::User, "hello".to_string())],
        options: ModelOptions::default(),
        tools: Vec::new(),
        tool_choice: ModelToolChoice::Auto,
        stream: false,
    };

    let result = turn_executor
        .run_with_handler(request, handler)
        .await
        .unwrap();

    assert_eq!(result.final_message.text_content().as_str(), "done");
    let observed = observed.lock().unwrap();
    assert!(matches!(
        observed.first(),
        Some(AgentRawEvent::AgentStarted { .. })
    ));
    assert!(matches!(
        observed.last(),
        Some(AgentRawEvent::AgentCompleted { .. })
    ));
    assert!(observed.iter().any(|event| matches!(
        event,
        AgentRawEvent::StateChanged {
            transition: aicoder_core::AgentStateTransition {
                current: AgentRunState::VerifyingCompletion { round: 1 },
                ..
            }
        }
    )));
}

#[tokio::test]
async fn external_application_can_run_a_persisted_turn() {
    let workspace = tempdir().unwrap();
    let repository = MemorySessionRepository::new();
    let turn_executor = TurnExecutor::builder(StaticProvider)
        .workspace(workspace.path())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
    let agent = Agent::new(
        turn_executor,
        AgentConfig::new("test-model").system_prompt("system context"),
    );

    let result = agent
        .run_with_session(&repository, SessionSelection::New, "hello", Arc::new(()))
        .await
        .unwrap();

    let session = result.session.unwrap();
    assert_eq!(
        result
            .execution_result
            .final_message
            .text_content()
            .as_str(),
        "done"
    );
    assert_eq!(repository.open(&session.id).unwrap().messages().len(), 2);
}

#[test]
fn external_application_can_manage_conversation_sessions() {
    let workspace = tempdir().unwrap();
    let repository = MemorySessionRepository::new();
    let mut session = repository.create(workspace.path()).unwrap();
    repository
        .append(
            &mut session,
            Message::text(MessageRole::User, "remember this".to_string()),
        )
        .unwrap();

    let reopened = repository.open(&session.metadata().id).unwrap();
    assert_eq!(reopened.chat_messages().len(), 1);
    assert_eq!(
        repository
            .most_recent(workspace.path())
            .unwrap()
            .unwrap()
            .id,
        session.metadata().id
    );
}

#[tokio::test]
async fn external_application_can_cancel_a_turn() {
    let workspace = tempdir().unwrap();
    let turn_executor = TurnExecutor::builder(StaticProvider)
        .workspace(workspace.path())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
    let context = TurnExecutionContext::new();
    context.cancel("application shutdown");

    let error = turn_executor
        .run_with_context(
            ModelRequest {
                model: "test-model".to_string(),
                messages: Vec::new(),
                options: ModelOptions::default(),
                tools: Vec::new(),
                tool_choice: ModelToolChoice::Auto,
                stream: false,
            },
            Arc::new(()),
            context,
        )
        .await
        .unwrap_err();

    assert_eq!(
        error.downcast_ref::<TurnCancelled>().unwrap().reason(),
        "application shutdown"
    );
}

#[tokio::test]
async fn external_application_can_observe_a_structured_cancelled_outcome() {
    let workspace = tempdir().unwrap();
    let turn_executor = TurnExecutor::builder(StaticProvider)
        .workspace(workspace.path())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
    let agent = Agent::new(turn_executor, AgentConfig::new("test-model"));
    let context = TurnExecutionContext::new();
    context.cancel("frontend cancelled");

    let outcome = agent.run_outcome("question", Arc::new(()), context).await;

    let AgentTurnOutcome::Aborted(interrupted) = outcome else {
        panic!("expected aborted outcome");
    };
    assert_eq!(
        interrupted
            .error
            .downcast_ref::<TurnCancelled>()
            .unwrap()
            .reason(),
        "frontend cancelled"
    );
    assert_eq!(interrupted.rounds, 0);
}

#[test]
fn repository_checkpoints_are_ephemeral_and_independent_between_cloned_views() {
    use aicoder_core::session::Session;

    #[derive(Debug, PartialEq)]
    struct Checkpoint(u64);

    let workspace = tempdir().unwrap();
    let repository = MemorySessionRepository::new();
    let mut session = repository.create(workspace.path()).unwrap();
    session.attach_backend_state(Checkpoint(1), Some(workspace.path().join("snapshot")));
    session.attach_run_token(Some("private-owner".into()));
    let old_view = session.clone();
    session.attach_backend_state(Checkpoint(2), None);
    assert_eq!(old_view.backend_state::<Checkpoint>(), Some(&Checkpoint(1)));
    assert_eq!(session.backend_state::<Checkpoint>(), Some(&Checkpoint(2)));
    assert!(old_view.path().is_some());
    assert!(session.path().is_none());
    assert!(session.backend_state::<String>().is_none());
    assert!(!format!("{old_view:?}").contains("Checkpoint(1)"));

    let restored =
        Session::from_entries(old_view.metadata().clone(), old_view.entries().to_vec()).unwrap();
    assert!(restored.backend_state::<Checkpoint>().is_none());
    assert!(restored.path().is_none());
    assert!(restored.run_token().is_none());
    assert_eq!(restored.metadata().revision, old_view.metadata().revision);
    session.clear_backend_state();
    assert!(session.backend_state::<Checkpoint>().is_none());
}
