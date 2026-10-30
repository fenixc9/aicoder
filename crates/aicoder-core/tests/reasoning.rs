use aicoder_core::{model::*, session::*, *};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
struct Provider(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.reasoning_efforts = match model {
            "m1" => vec![
                ReasoningEffort::None,
                ReasoningEffort::Low,
                ReasoningEffort::High,
            ],
            "m2" => vec![ReasoningEffort::Low],
            _ => vec![],
        };
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
#[test]
fn omission_explicit_none_and_declarations_are_distinct() {
    let provider = Provider(Arc::new(Mutex::new(vec![])));
    let mut request = ModelRequest::new("m1", vec![Message::text(MessageRole::User, "test")]);
    assert!(provider.validate_request(&request).is_ok());
    assert!(
        serde_json::to_value(&request.options)
            .unwrap()
            .get("reasoning_effort")
            .is_none()
    );
    request.options.reasoning_effort = Some(ReasoningEffort::None);
    assert!(provider.validate_request(&request).is_ok());
    assert_eq!(
        serde_json::to_value(&request.options).unwrap()["reasoning_effort"],
        "none"
    );
    request.model = "unknown".into();
    assert_eq!(
        provider.validate_request(&request).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
    let mut spec = provider.model_spec("m1").unwrap();
    spec.capabilities.reasoning_efforts = vec![ReasoningEffort::Low, ReasoningEffort::Low];
    assert!(spec.validate().is_err());
    let options: ModelOptions = serde_json::from_str("{}").unwrap();
    assert_eq!(options.reasoning_effort, None);
}
#[tokio::test]
async fn selections_are_validated_branch_local_and_do_not_leak_across_model_epochs() {
    let workspace = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(workspace.path()).unwrap();
    let id = session.metadata().id.clone();
    let calls = Arc::new(Mutex::new(vec![]));
    let agent = Agent::new(
        TurnExecutor::builder(Provider(calls.clone()))
            .workspace(workspace.path())
            .build()
            .unwrap(),
        AgentConfig::new("m1"),
    )
    .following_session_models()
    .following_session_reasoning();
    let report = agent
        .switch_session_reasoning(
            &repo,
            &id,
            Some(ReasoningEffort::High),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.reasoning_effort, Some(ReasoningEffort::High));
    assert!(calls.lock().unwrap().is_empty());
    let selected = repo.open(&id).unwrap();
    let anchor = selected.metadata().leaf_id.clone().unwrap();
    let revision = selected.metadata().revision;
    assert!(
        agent
            .switch_session_reasoning(
                &repo,
                &id,
                Some(ReasoningEffort::Max),
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert_eq!(repo.open(&id).unwrap().metadata().revision, revision);
    let result = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            "first",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(
        result
            .execution_result
            .progress
            .model_context
            .as_ref()
            .unwrap()
            .reasoning_effort,
        Some(ReasoningEffort::High)
    );
    assert_eq!(
        calls.lock().unwrap()[0].options.reasoning_effort,
        Some(ReasoningEffort::High)
    );
    agent
        .switch_session_model_with_reasoning(
            &repo,
            &id,
            "m2",
            Some(ReasoningEffort::Low),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    agent
        .switch_session_model(&repo, &id, "m1", TurnExecutionContext::new())
        .await
        .unwrap();
    assert!(
        repo.open(&id)
            .unwrap()
            .reasoning_selection()
            .unwrap()
            .is_none()
    );
    SessionNavigator::new(&repo, workspace.path())
        .unwrap()
        .execute(
            &id,
            ArchiveNavigation::Branch {
                source: BranchSource::Entry(anchor),
                name: None,
            },
        )
        .unwrap();
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            "fork",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .options
            .reasoning_effort,
        Some(ReasoningEffort::High)
    );
    assert!(
        repo.open(&id)
            .unwrap()
            .entries()
            .iter()
            .any(|e| matches!(&e.data,SessionEntryData::ModelChanged{model,..} if model=="m2"))
    );
    let before = repo.open(&id).unwrap().metadata().revision;
    let cancelled = TurnExecutionContext::new();
    cancelled.cancel("stop");
    assert!(
        agent
            .switch_session_reasoning(&repo, &id, None, cancelled)
            .await
            .is_err()
    );
    assert_eq!(repo.open(&id).unwrap().metadata().revision, before);
}
#[tokio::test]
async fn explicit_default_is_durable_and_bad_metadata_never_becomes_instructions() {
    let workspace = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(workspace.path()).unwrap();
    let id = session.metadata().id.clone();
    assert!(
        repo.append_entries(
            &mut session,
            vec![ReasoningSelection::new("fixture", "m1", Some(ReasoningEffort::High)).data()]
        )
        .is_err()
    );
    let calls = Arc::new(Mutex::new(vec![]));
    let executor = || {
        TurnExecutor::builder(Provider(calls.clone()))
            .workspace(workspace.path())
            .build()
            .unwrap()
    };
    let first = Agent::new(executor(), AgentConfig::new("m1")).recording_reasoning_defaults();
    first
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            "default choice",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(
        repo.open(&id)
            .unwrap()
            .reasoning_selection()
            .unwrap()
            .unwrap()
            .effort,
        None
    );
    let mut config = AgentConfig::new("m1");
    config.options.reasoning_effort = Some(ReasoningEffort::High);
    Agent::new(executor(), config)
        .following_session_reasoning()
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id),
            "next",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.last().unwrap().options.reasoning_effort, None);
    assert!(
        !calls
            .last()
            .unwrap()
            .messages
            .iter()
            .any(|m| m.text_content().contains(REASONING_NAMESPACE))
    );
}

#[tokio::test]
async fn an_explicit_model_selection_can_rebind_old_provider_metadata_without_routing_from_it() {
    let workspace = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(workspace.path()).unwrap();
    let id = session.metadata().id.clone();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::ModelChanged {
                provider: "old-route".into(),
                model: "m1".into(),
            },
            ReasoningSelection::new("old-route", "m1", Some(ReasoningEffort::High)).data(),
        ],
    )
    .unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let agent = Agent::new(
        TurnExecutor::builder(Provider(calls.clone()))
            .workspace(workspace.path())
            .build()
            .unwrap(),
        AgentConfig::new("m1"),
    )
    .following_session_models()
    .following_session_reasoning();
    let report = agent
        .switch_session_model(&repo, &id, "m1", TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(report.spec.provider, "fixture");
    let stored = repo.open(&id).unwrap();
    assert_eq!(stored.selected_model(), Some(("fixture", "m1")));
    assert!(stored.reasoning_selection().unwrap().is_none());
    assert!(calls.lock().unwrap().is_empty());
}
