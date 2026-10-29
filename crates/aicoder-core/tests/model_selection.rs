use aicoder_core::{model::*, session::*, *};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
struct Provider {
    name: &'static str,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new(self.name, model);
        spec.capabilities.tools = model != "no-tools";
        spec.capabilities.images = model == "vision";
        spec.capabilities.reasoning_history = model == "reasoning";
        if model == "tiny" {
            spec.context_tokens = Some(100);
            spec.max_output_tokens = Some(20);
        }
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let model = request.model.clone();
        self.requests.lock().unwrap().push(request);
        let response = ModelResponse {
            id: None,
            model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
fn agent(
    root: &std::path::Path,
    provider: &'static str,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
) -> Agent {
    Agent::new(
        TurnExecutor::builder(Provider {
            name: provider,
            requests,
        })
        .workspace(root)
        .build()
        .unwrap(),
        AgentConfig::new("default"),
    )
}
#[tokio::test]
async fn branch_model_is_followed_and_a_checkout_restores_the_previous_selection() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append(&mut session, Message::text(MessageRole::User, "root"))
        .unwrap();
    let original = session.metadata().leaf_id.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = agent(root.path(), "one", requests.clone()).following_session_models();
    let report = agent
        .switch_session_model(
            &repo,
            &session.metadata().id,
            "selected",
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.spec.id, "selected");
    assert!(requests.lock().unwrap().is_empty());
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "continue",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "{outcome:?}"
    );
    assert_eq!(requests.lock().unwrap()[0].model, "selected");
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    nav.checkout(&session.metadata().id, original.as_deref())
        .unwrap();
    agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "old branch",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert_eq!(requests.lock().unwrap()[1].model, "default");
}
#[tokio::test]
async fn image_and_opaque_provider_incompatibilities_fail_before_any_session_write() {
    for block in [
        ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.invalid/image.png".into(),
            },
        },
        ContentBlock::ProviderData {
            provider: "foreign".into(),
            data: json!({"opaque":"private"}),
        },
    ] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let mut session = repo.create(root.path()).unwrap();
        repo.append(
            &mut session,
            Message {
                role: MessageRole::User,
                content: vec![block],
            },
        )
        .unwrap();
        let before = serde_json::to_value(session.entries()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let agent = agent(root.path(), "one", requests.clone());
        assert!(
            agent
                .switch_session_model(
                    &repo,
                    &session.metadata().id,
                    "text",
                    TurnExecutionContext::new()
                )
                .await
                .is_err()
        );
        let outcome = agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "continue",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
        let saved = repo.open(&session.metadata().id).unwrap();
        assert_eq!(saved.metadata(), session.metadata());
        assert_eq!(serde_json::to_value(saved.entries()).unwrap(), before);
        assert!(requests.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn following_a_foreign_provider_requires_explicit_host_routing() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    agent(root.path(), "one", requests.clone())
        .switch_session_model(
            &repo,
            &session.metadata().id,
            "first",
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let before = repo.open(&session.metadata().id).unwrap();
    let other = agent(root.path(), "two", requests.clone()).following_session_models();
    let outcome = other
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "continue",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
    assert_eq!(
        repo.open(&session.metadata().id).unwrap().metadata(),
        before.metadata()
    );
    assert!(requests.lock().unwrap().is_empty());
    other
        .switch_session_model(
            &repo,
            &session.metadata().id,
            "second",
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    other
        .run_with_session(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "explicitly routed",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert_eq!(requests.lock().unwrap()[0].model, "second");
}
struct Context {
    calls: Arc<AtomicUsize>,
    pending: bool,
}
#[async_trait]
impl InputContextProvider for Context {
    async fn prepare(&self, _: &std::path::Path, _: &[Message]) -> anyhow::Result<PreparedInput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.pending {
            return std::future::pending().await;
        }
        Ok(PreparedInput {
            system_prefix: vec![Message::text(MessageRole::System, "frozen once")],
            provenance: json!({"source":"fixture"}),
        })
    }
}
#[tokio::test]
async fn preflight_freezes_input_once_and_keeps_reasoning_in_canonical_history() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append(
        &mut session,
        Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Reasoning {
                text: "retained reasoning".into(),
            }],
        },
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let prepared = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        TurnExecutor::builder(Provider {
            name: "one",
            requests: requests.clone(),
        })
        .workspace(root.path())
        .input_context(Arc::new(Context {
            calls: prepared.clone(),
            pending: false,
        }))
        .build()
        .unwrap(),
        AgentConfig::new("text"),
    );
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "continue",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "{outcome:?}"
    );
    assert_eq!(prepared.load(Ordering::SeqCst), 1);
    assert_eq!(
        requests.lock().unwrap()[0].messages[0].text_content(),
        "frozen once"
    );
    assert!(
        !serde_json::to_string(&*requests.lock().unwrap())
            .unwrap()
            .contains("retained reasoning")
    );
    assert!(
        serde_json::to_string(repo.open(&session.metadata().id).unwrap().entries())
            .unwrap()
            .contains("retained reasoning")
    );
}
#[tokio::test(start_paused = true)]
async fn slow_preflight_hits_duration_budget_without_writing_a_turn_or_calling_the_model() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        TurnExecutor::builder(Provider {
            name: "one",
            requests: requests.clone(),
        })
        .workspace(root.path())
        .input_context(Arc::new(Context {
            calls: Arc::new(AtomicUsize::new(0)),
            pending: true,
        }))
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_duration: Some(std::time::Duration::from_millis(10)),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap(),
        AgentConfig::new("text"),
    );
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "continue",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Limited(_)),
        "{outcome:?}"
    );
    assert_eq!(
        repo.open(&session.metadata().id).unwrap().metadata(),
        session.metadata()
    );
    assert!(requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn small_known_model_window_rejects_oversized_history_and_invalid_output_options() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append(
        &mut session,
        Message::text(MessageRole::User, "x".repeat(1000)),
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = agent(root.path(), "one", requests.clone());
    assert!(
        agent
            .switch_session_model(
                &repo,
                &session.metadata().id,
                "tiny",
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert_eq!(
        repo.open(&session.metadata().id).unwrap().metadata(),
        session.metadata()
    );
    assert!(requests.lock().unwrap().is_empty());
}

struct Tool;
#[async_trait]
impl aicoder_core::tools::ExecutableTool for Tool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "work".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> aicoder_core::tools::ToolCapability {
        aicoder_core::tools::ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        _: &aicoder_core::tools::ToolContext,
        _: serde_json::Value,
    ) -> Result<aicoder_core::tools::ToolSuccess, aicoder_core::tools::ToolFailure> {
        panic!("preflight never executes tools")
    }
}
#[tokio::test]
async fn registered_tools_output_limits_and_incompatible_context_policies_are_checked() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut registry = aicoder_core::tools::ToolRegistry::default();
    registry.register(Tool).unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(Provider {
            name: "one",
            requests: requests.clone(),
        })
        .workspace(root.path())
        .registry(registry)
        .build()
        .unwrap(),
        AgentConfig::new("default"),
    );
    assert!(
        agent
            .switch_session_model(
                &repo,
                &session.metadata().id,
                "no-tools",
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    let mut config = AgentConfig::new("default");
    config.options.max_output_tokens = Some(50);
    let agent = Agent::new(
        TurnExecutor::builder(Provider {
            name: "one",
            requests: requests.clone(),
        })
        .workspace(root.path())
        .build()
        .unwrap(),
        config,
    );
    assert!(
        agent
            .switch_session_model(
                &repo,
                &session.metadata().id,
                "tiny",
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    let policy = PruningContextCompactor::new(ContextWindowConfig {
        max_context_tokens: 1000,
        reserved_output_tokens: 20,
        preserve_recent_tokens: 10,
    })
    .unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(Provider {
            name: "one",
            requests: requests.clone(),
        })
        .workspace(root.path())
        .context_compactor(policy)
        .build()
        .unwrap(),
        AgentConfig::new("default"),
    );
    assert!(
        agent
            .switch_session_model(
                &repo,
                &session.metadata().id,
                "tiny",
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert_eq!(
        repo.open(&session.metadata().id).unwrap().metadata(),
        session.metadata()
    );
    assert!(requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn valid_vision_model_is_selected_and_active_leases_or_cancellation_do_not_mutate_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append(
        &mut session,
        Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.invalid/image.png".into(),
                },
            }],
        },
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = agent(root.path(), "one", requests.clone());
    let lease = repo.acquire_run(&mut session).unwrap();
    assert!(
        agent
            .switch_session_model(
                &repo,
                &session.metadata().id,
                "vision",
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    drop(lease);
    let context = TurnExecutionContext::new();
    context.cancel("cancel selection");
    assert!(
        agent
            .switch_session_model(&repo, &session.metadata().id, "vision", context)
            .await
            .is_err()
    );
    assert_eq!(
        repo.open(&session.metadata().id).unwrap().metadata(),
        session.metadata()
    );
    agent
        .switch_session_model(
            &repo,
            &session.metadata().id,
            "vision",
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let outcome = agent
        .following_session_models()
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "inspect",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "{outcome:?}"
    );
    assert_eq!(requests.lock().unwrap()[0].model, "vision");
}

struct BatchOnly(Provider);
#[async_trait]
impl ModelProvider for BatchOnly {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        self.0.model_spec(model)
    }
    fn validate_request(&self, request: &ModelRequest) -> Result<(), ProviderError> {
        if request.stream {
            return Err(ProviderError::new(
                "one",
                ProviderErrorKind::Unsupported,
                "batch only",
            ));
        }
        request.validate(&self.model_spec(&request.model)?)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        context: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.0.generate(request, sink, context).await
    }
}
#[tokio::test]
async fn preflight_checks_the_actual_host_stream_mode() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        TurnExecutor::builder(BatchOnly(Provider {
            name: "one",
            requests: requests.clone(),
        }))
        .workspace(root.path())
        .config(TurnExecutionConfig {
            stream: false,
            ..Default::default()
        })
        .build()
        .unwrap(),
        AgentConfig::new("default"),
    )
    .following_session_models();
    agent
        .switch_session_model(
            &repo,
            &session.metadata().id,
            "selected",
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "continue",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert!(!requests.lock().unwrap()[0].stream);
}
