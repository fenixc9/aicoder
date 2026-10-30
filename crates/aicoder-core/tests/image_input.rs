use aicoder_core::{model::*, session::*, tokens::*, *};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

fn image(data: &str) -> Message {
    Message {
        role: MessageRole::User,
        content: vec![ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/jpeg".into(),
                data: data.into(),
            },
        }],
    }
}
struct Provider(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl ModelProvider for Provider {
    fn validate_request(&self, request: &ModelRequest) -> Result<(), ProviderError> {
        request.validate(&self.model_spec(&request.model)?)?;
        if request
            .messages
            .first()
            .is_none_or(|m| m.role != MessageRole::System || m.text_content() != "host prefix")
        {
            return Err(ProviderError::new(
                "fixture",
                ProviderErrorKind::InvalidRequest,
                "adapter requires the host prefix in every request",
            ));
        }
        Ok(())
    }
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.images = model.starts_with("vision");
        if model == "vision-small" {
            spec.context_tokens = Some(4096);
        }
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.0.lock().unwrap().push(request.clone());
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
fn agent(root: &std::path::Path, model: &str, calls: Arc<Mutex<Vec<ModelRequest>>>) -> Agent {
    Agent::new(
        TurnExecutor::builder(Provider(calls))
            .workspace(root)
            .build()
            .unwrap(),
        AgentConfig::new(model).system_prompt("host prefix"),
    )
}
#[test]
fn input_boundary_preserves_text_and_rejects_non_user_blocks_and_image_overflow() {
    assert_eq!(
        AgentInput::from("text").into_message(),
        Message::text(MessageRole::User, "text")
    );
    let mut message = image("eA==");
    assert_eq!(
        AgentInput::try_from(message.clone())
            .unwrap()
            .into_message(),
        message
    );
    for role in [
        MessageRole::System,
        MessageRole::Assistant,
        MessageRole::Tool,
    ] {
        message.role = role;
        assert!(AgentInput::try_from(message.clone()).is_err());
    }
    message.role = MessageRole::User;
    message.content = vec![ContentBlock::Reasoning {
        text: "not input".into(),
    }];
    assert!(AgentInput::try_from(message).is_err());
    let mut message = image("eA==");
    message.content = vec![message.content[0].clone(); 5];
    assert!(AgentInput::try_from(message).is_err());
    assert!(AgentInput::try_from(image(&"a".repeat(2 * 1024 * 1024))).is_err());
    for url in [
        "file:///private",
        "https://user:secret@example.com/a",
        "data:image/png,a",
    ] {
        assert!(
            AgentInput::try_from(Message {
                role: MessageRole::User,
                content: vec![ContentBlock::Image {
                    source: ImageSource::Url { url: url.into() }
                }]
            })
            .is_err()
        );
    }
}
#[test]
fn image_counts_are_explicit_estimates_independent_of_base64_transport_size() {
    let spec = ModelSpec::new("fixture", "vision");
    let count = |message| {
        TokenCounting::default().request(&ModelRequest::new("vision", vec![message]), &spec)
    };
    let small = count(image("eA=="));
    let large = count(image(&"a".repeat(1024 * 1024)));
    assert_eq!(small, large);
    assert!(small.tokens >= 8192);
    assert_eq!(small.accuracy, TokenAccuracy::Estimated);
    assert_eq!(small.fallback, Some(TokenFallback::UnsupportedContent));
    assert_eq!(small.method, "heuristic_v1_images");
}
#[tokio::test]
async fn sdk_images_survive_stateless_session_replay_and_incompatible_switch_is_read_only() {
    let root = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let agent = agent(root.path(), "vision", calls.clone());
    let message = image("eA==");
    agent
        .run(AgentInput::try_from(message.clone()).unwrap(), Arc::new(()))
        .await
        .unwrap();
    assert_eq!(calls.lock().unwrap()[0].messages.last(), Some(&message));
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            AgentInput::try_from(message.clone()).unwrap(),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(repo.open(&id).unwrap().messages()[0].message, &message);
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            "next",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .messages
            .contains(&message)
    );
    let before = repo.open(&id).unwrap().metadata().revision;
    assert!(
        agent
            .switch_session_model(&repo, &id, "text", TurnExecutionContext::new())
            .await
            .is_err()
    );
    assert_eq!(repo.open(&id).unwrap().metadata().revision, before);
    assert_eq!(calls.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn unsupported_initial_and_queued_images_do_not_become_applied_history() {
    let root = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let agent = agent(root.path(), "text", calls.clone());
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let before = session.metadata().revision;
    let result = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            AgentInput::try_from(image("eA==")).unwrap(),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(result.into_result().is_err());
    assert_eq!(repo.open(&id).unwrap().metadata().revision, before);
    let (handle, control) = RunHandle::pair();
    let pending = handle.steer(image("eA==")).unwrap();
    let result = agent
        .run_with_session_controlled(
            &repo,
            SessionSelection::Existing(id.clone()),
            "initial text",
            Arc::new(()),
            control,
        )
        .await;
    assert_eq!(result.progress().pending_inputs[0].id, pending);
    assert!(result.into_result().is_err());
    assert_eq!(
        repo.open(&id)
            .unwrap()
            .messages()
            .iter()
            .map(|m| m.message.clone())
            .collect::<Vec<_>>(),
        vec![Message::text(MessageRole::User, "initial text")]
    );
    assert_eq!(handle.snapshot().pending_inputs[0].id, pending);
    assert!(calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn accepted_steer_and_follow_up_use_the_same_image_message_contract() {
    let root = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let agent = agent(root.path(), "vision", calls.clone());
    let (handle, control) = RunHandle::pair();
    let message = image("eA==");
    handle.steer(message.clone()).unwrap();
    handle.follow_up(message.clone()).unwrap();
    agent
        .run_controlled("initial", Arc::new(()), control)
        .await
        .into_result()
        .unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].messages.last(), Some(&message));
    assert_eq!(calls[1].messages.last(), Some(&message));
    assert!(handle.snapshot().pending_inputs.is_empty());
}

#[tokio::test]
async fn image_estimates_still_enforce_known_windows_before_any_turn_write() {
    let root = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let agent = agent(root.path(), "vision-small", calls.clone());
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let before = session.metadata().revision;
    let result = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            AgentInput::try_from(image("eA==")).unwrap(),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(result.into_result().is_err());
    assert_eq!(repo.open(&id).unwrap().metadata().revision, before);
    assert!(calls.lock().unwrap().is_empty());
}
