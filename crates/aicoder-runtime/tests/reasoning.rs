use aicoder_core::{model::*, session::*, *};
use aicoder_runtime::{
    resources::ResourceSettings,
    session::{JsonlSessionRepository, SessionLoadLimits},
    *,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, Mutex},
};
fn profile() -> Value {
    json!({"version":1,"profiles":{"work":{"base_url":"http://127.0.0.1:1/v1","api_key_env":"FIXTURE_KEY","provider_id":"fixture","model":"m1","proxy":"disabled","models":[{"provider":"fixture","id":"m1","capabilities":{"tools":true,"reasoning_efforts":["none","low","high"]}}]}},"settings":{"provider_profile":"work"}})
}
#[test]
fn trusted_metadata_and_nullable_precedence_are_explicit_without_resolving_credentials() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let file = root.path().join("settings.json");
    fs::write(&file, profile().to_string()).unwrap();
    fs::create_dir(workspace.path().join(".aicoder")).unwrap();
    fs::write(
        workspace.path().join(".aicoder/settings.json"),
        json!({"version":1,"settings":{"options":{"reasoning_effort":"low"}}}).to_string(),
    )
    .unwrap();
    let environment = Arc::new(BTreeMap::from([
        (
            "AICODER_HOME".into(),
            root.path().to_string_lossy().into_owned(),
        ),
        ("AICODER_REASONING_EFFORT".into(), "high".into()),
    ]));
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        ConfigDiscovery::default(),
        environment.clone(),
    )
    .unwrap();
    assert_eq!(
        runtime.config().options.reasoning_effort,
        Some(ReasoningEffort::High)
    );
    assert!(!runtime.follows_session_reasoning());
    assert_eq!(
        runtime
            .session_client()
            .unwrap()
            .model_spec("m1")
            .unwrap()
            .capabilities
            .reasoning_efforts
            .len(),
        3
    );
    let cleared = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                options: ModelSettings {
                    reasoning_effort: Setting::Clear,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        ConfigDiscovery::default(),
        environment,
    )
    .unwrap();
    assert_eq!(cleared.config().options.reasoning_effort, None);
    assert!(!cleared.follows_session_reasoning());
    let snapshot = serde_json::to_string(runtime.config()).unwrap();
    assert!(snapshot.contains("FIXTURE_KEY"));
    assert!(!snapshot.contains("api_key\":\""));
    fs::write(
        workspace.path().join(".aicoder/settings.json"),
        profile().to_string(),
    )
    .unwrap();
    assert!(
        Runtime::load_with_environment(
            workspace.path(),
            RuntimeOverrides::default(),
            ConfigDiscovery::default(),
            Arc::new(BTreeMap::from([(
                "AICODER_HOME".into(),
                root.path().to_string_lossy().into_owned()
            )]))
        )
        .is_err()
    );
}
struct Provider(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.tools = true;
        spec.capabilities.reasoning_efforts = vec![ReasoningEffort::Low, ReasoningEffort::High];
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
#[tokio::test]
async fn archive_windows_retain_branch_reasoning_without_turning_it_into_text() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::ModelChanged {
                provider: "fixture".into(),
                model: "m1".into(),
            },
            ReasoningSelection::new("fixture", "m1", Some(ReasoningEffort::High)).data(),
        ],
    )
    .unwrap();
    for i in 0..20 {
        repo.append(
            &mut session,
            Message::text(MessageRole::User, format!("{i} {}", "x".repeat(2000))),
        )
        .unwrap();
    }
    let reader = JsonlSessionRepository::with_limits(
        root.path(),
        SessionLoadLimits {
            max_snapshot_bytes: 1024,
            max_window_bytes: 4096,
            ..Default::default()
        },
    )
    .unwrap();
    let window = reader.open_run(&session.metadata().id).unwrap();
    assert!(matches!(window, RunSession::Window(_)));
    assert_eq!(
        window.reasoning_selection().unwrap().unwrap().effort,
        Some(ReasoningEffort::High)
    );
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
    agent
        .run_with_session_outcome(
            &reader,
            SessionSelection::Existing(session.metadata().id.clone()),
            "next",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(
        calls.lock().unwrap()[0].options.reasoning_effort,
        Some(ReasoningEffort::High)
    );
    assert!(
        !calls.lock().unwrap()[0]
            .messages
            .iter()
            .any(|m| m.text_content().contains(REASONING_NAMESPACE))
    );
    agent
        .switch_session_reasoning(
            &reader,
            &session.metadata().id,
            Some(ReasoningEffort::Low),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        reader
            .open_run(&session.metadata().id)
            .unwrap()
            .reasoning_selection()
            .unwrap()
            .unwrap()
            .effort,
        Some(ReasoningEffort::Low)
    );
}

#[tokio::test]
async fn changing_only_request_preferences_keeps_the_live_registry_and_session_grants() {
    use aicoder_core::tools::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Writer(AtomicUsize);
    #[async_trait]
    impl ModelProvider for Writer {
        fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
            let mut spec = ModelSpec::new("fixture", model);
            spec.capabilities.tools = true;
            spec.capabilities.reasoning_efforts = vec![ReasoningEffort::High];
            Ok(spec)
        }
        async fn generate(
            &self,
            request: ModelRequest,
            sink: Arc<dyn ModelEventSink>,
            _: TurnExecutionContext,
        ) -> Result<ModelResponse, ProviderError> {
            let step = self.0.fetch_add(1, Ordering::SeqCst);
            let tool = request.messages.last().unwrap().role != MessageRole::Tool;
            let message = if tool {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: format!("write-{step}"),
                            name: "write_file".into(),
                            arguments: json!({"path":"result.txt","content":format!("version-{step}")}),
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::Assistant, "done")
            };
            let response = ModelResponse {
                id: None,
                model: request.model,
                message,
                stop_reason: if tool {
                    StopReason::ToolCalls
                } else {
                    StopReason::Complete
                },
                usage: ModelUsage::zero(),
            };
            emit_response(&response, sink.as_ref());
            Ok(response)
        }
    }
    struct Approve(Arc<AtomicUsize>);
    #[async_trait]
    impl ApprovalHandler for Approve {
        async fn decide(&self, request: &ApprovalRequest<'_>) -> anyhow::Result<ApprovalDecision> {
            assert!(request.session.is_some());
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ApprovalDecision::AllowSession)
        }
    }
    let workspace = tempfile::tempdir().unwrap();
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Clear,
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::new()),
    )
    .unwrap();
    let approvals = Arc::new(AtomicUsize::new(0));
    let base = Agent::new(
        runtime
            .executor_builder(Arc::new(Writer(AtomicUsize::new(0))))
            .unwrap()
            .approval(Approve(approvals.clone()))
            .build()
            .unwrap(),
        AgentConfig::new("m1"),
    )
    .following_session_models()
    .following_session_reasoning();
    let repo = MemorySessionRepository::new();
    let authority = ApprovalSessions::default();
    let first = base
        .with_reasoning_effort(Some(ReasoningEffort::High))
        .run_with_session_outcome(
            &repo,
            SessionSelection::New,
            "one",
            Arc::new(()),
            TurnExecutionContext::new().with_approval_sessions(authority.clone()),
        )
        .await
        .into_result()
        .unwrap();
    base.run_with_session_outcome(
        &repo,
        SessionSelection::Existing(first.session.unwrap().id),
        "two",
        Arc::new(()),
        TurnExecutionContext::new().with_approval_sessions(authority),
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    assert_eq!(
        fs::read_to_string(workspace.path().join("result.txt")).unwrap(),
        "version-2"
    );
}
