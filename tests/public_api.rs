use std::sync::{Arc, Mutex};

use aicoder_core::{
    Agent, AgentRawEvent, AgentRunState, AgentWorkflow, AgentWorkflowConfig,
    ChatCompletionProvider, SessionSelection,
    session::{MemorySessionRepository, SessionRepository},
    tools::ToolRegistry,
    types::{ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Role},
};
use anyhow::Result;
use async_trait::async_trait;
use tempfile::tempdir;

struct StaticProvider;

#[async_trait]
impl ChatCompletionProvider for StaticProvider {
    async fn complete(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        Ok(serde_json::from_value(serde_json::json!({
            "id": "response-1",
            "object": "chat.completion",
            "created": 1,
            "model": request.model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "done"},
                "finish_reason": "stop"
            }]
        }))?)
    }
}

#[tokio::test]
async fn external_application_can_build_agent_and_observe_raw_events() {
    let workspace = tempdir().unwrap();
    let agent = Agent::builder(StaticProvider)
        .workspace(workspace.path())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let target = Arc::clone(&observed);
    let handler = Arc::new(move |event: &aicoder_core::events::AgentRawEventEnvelope| {
        target.lock().unwrap().push(event.event.clone());
    });
    let request = ChatCompletionRequest {
        model: "test-model".to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: Some("hello".to_string()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        temperature: None,
        top_p: None,
        max_tokens: None,
        seed: None,
        tools: None,
        tool_choice: None,
        stream: None,
        stream_options: None,
        stop: None,
        response_format: None,
    };

    let result = agent.run_with_handler(request, handler).await.unwrap();

    assert_eq!(result.final_message.content.as_deref(), Some("done"));
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
async fn external_application_can_run_the_conversation_workflow() {
    let workspace = tempdir().unwrap();
    let repository = MemorySessionRepository::new();
    let agent = Agent::builder(StaticProvider)
        .workspace(workspace.path())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
    let workflow = AgentWorkflow::new(
        agent,
        AgentWorkflowConfig::new("test-model").system_prompt("system context"),
    );

    let result = workflow
        .run_with_session(&repository, SessionSelection::New, "hello", Arc::new(()))
        .await
        .unwrap();

    let session = result.session.unwrap();
    assert_eq!(result.run.final_message.content.as_deref(), Some("done"));
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
            ChatMessage {
                role: Role::User,
                content: Some("remember this".to_string()),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
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
