use aicoder_core::{events::*, extensions::*, model::*, session::*, tools::*, *};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[derive(Default)]
struct Provider {
    requests: Mutex<Vec<ModelRequest>>,
    call_hidden: bool,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.tools = true;
        spec.context_tokens = Some(16384);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let first = self.requests.lock().unwrap().is_empty();
        self.requests.lock().unwrap().push(request.clone());
        let calls = first && self.call_hidden;
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: if calls {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "call".into(),
                            name: "hidden".into(),
                            arguments: json!({}),
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::Assistant, "answer")
            },
            stop_reason: if calls {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage {
                input_tokens: Some(5),
                output_tokens: Some(2),
                total_tokens: Some(7),
                cached_input_tokens: Some(0),
                cache_write_tokens: Some(0),
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
fn extensions(hook: Arc<dyn BeforeStartHook>) -> Arc<ExtensionSet> {
    let mut e = Extension::new("startup", "1").unwrap();
    e.before_start_hook(HookSpec::new("prepare"), hook).unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    Arc::new(r.finish())
}
#[derive(Clone, Copy)]
enum Mode {
    Valid,
    Empty,
    BadPrefix,
    Huge,
    Count,
    Fail,
    Panic,
}
struct Hook {
    mode: Mode,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl BeforeStartHook for Hook {
    async fn before_start(
        &self,
        input: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let action = match self.mode {
            Mode::Valid => BeforeStartAction {
                system_prefix: Some(vec![Message::text(
                    MessageRole::System,
                    "PRIVATE_STARTUP_PREFIX",
                )]),
                append: vec![
                    BoundaryEntry::Context {
                        kind: "note".into(),
                        text: "EXPLICIT_STARTUP_FACT".into(),
                    },
                    BoundaryEntry::Data {
                        key: "inert".into(),
                        value: json!("INERT_NOT_IN_PROMPT"),
                    },
                ],
                ..Default::default()
            },
            Mode::Empty => BeforeStartAction {
                tools: Some(vec![]),
                ..Default::default()
            },
            Mode::BadPrefix => BeforeStartAction {
                system_prefix: Some(vec![Message::text(MessageRole::User, "not a prefix")]),
                ..Default::default()
            },
            Mode::Huge => BeforeStartAction {
                system_prefix: Some(vec![Message::text(MessageRole::System, "x".repeat(65536))]),
                ..Default::default()
            },
            Mode::Count => BeforeStartAction {
                append: (0..33)
                    .map(|_| BoundaryEntry::Data {
                        key: "many".into(),
                        value: json!(0),
                    })
                    .collect(),
                ..Default::default()
            },
            Mode::Fail => anyhow::bail!("PRIVATE_CALLBACK_ERROR"),
            Mode::Panic => panic!("PRIVATE_CALLBACK_PANIC"),
        };
        assert_eq!(input.request.model, "m");
        Ok(Some(action))
    }
}
fn hook(mode: Mode) -> (Arc<dyn BeforeStartHook>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(Hook {
            mode,
            calls: calls.clone(),
        }),
        calls,
    )
}
fn request() -> ModelRequest {
    ModelRequest::new(
        "m",
        vec![
            Message::text(MessageRole::System, "ORIGINAL_PREFIX"),
            Message::text(MessageRole::User, "literal input"),
        ],
    )
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, e: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(e.clone());
    }
}
#[tokio::test]
async fn startup_changes_once_before_provider_io_and_keeps_history_and_receipts_separate() {
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(Provider::default());
    let (h, calls) = hook(Mode::Valid);
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(extensions(h))
        .build()
        .unwrap();
    let events = Arc::new(Events::default());
    let result = executor
        .run_outcome(request(), events.clone(), TurnExecutionContext::new())
        .await
        .into_result()
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.usage.total_tokens, Some(7));
    assert_eq!(result.final_message.text_content(), "answer");
    let requests = provider.requests.lock().unwrap();
    assert_eq!(
        requests[0].messages[0].text_content(),
        "PRIVATE_STARTUP_PREFIX"
    );
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.text_content().contains("EXPLICIT_STARTUP_FACT"))
    );
    assert!(
        !serde_json::to_string(&requests[0])
            .unwrap()
            .contains("INERT_NOT_IN_PROMPT")
    );
    assert_eq!(
        result
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .count(),
        1
    );
    assert_eq!(result.messages[1].text_content(), "literal input");
    let events = events.0.lock().unwrap();
    let mut validator = EventTraceValidator::new();
    for e in events.iter() {
        validator.observe(e).unwrap();
    }
    validator.finish().unwrap();
    assert!(events.iter().any(|e| matches!(
        e.event,
        AgentRawEvent::LifecycleBoundaryCommitted {
            boundary: BoundaryKind::BeforeStart,
            ..
        }
    ) && e.round.is_none()));
    assert!(
        !serde_json::to_string(&*events)
            .unwrap()
            .contains("PRIVATE_STARTUP_PREFIX")
    );
}
#[tokio::test]
async fn session_initial_batch_records_source_context_once_and_restore_never_restores_prompt_override()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let provider = Arc::new(Provider::default());
    let (h, calls) = hook(Mode::Valid);
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(extensions(h))
            .build()
            .unwrap(),
        AgentConfig::new("m").system_prompt("original"),
    );
    agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "literal input",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let saved = repo.open(&session.metadata().id).unwrap();
    assert!(saved.unfinished_turn().is_none());
    assert_eq!(saved.messages().len(), 3);
    let encoded = serde_json::to_string(saved.entries()).unwrap();
    assert!(!encoded.contains("PRIVATE_STARTUP_PREFIX"));
    assert!(encoded.contains("before_start"));
    assert!(encoded.contains("INERT_NOT_IN_PROMPT"));
    assert_eq!(
        saved
            .messages()
            .iter()
            .filter(|m| m.message.role == MessageRole::User)
            .count(),
        1
    );
    let second = Arc::new(Provider::default());
    let restore = Agent::new(
        TurnExecutor::builder_from_shared(second.clone())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("m").system_prompt("RESTORED_HOST_PREFIX"),
    );
    restore
        .run_with_session(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "next",
            Arc::new(()),
        )
        .await
        .unwrap();
    let requests = second.requests.lock().unwrap();
    assert_eq!(
        requests[0].messages[0].text_content(),
        "RESTORED_HOST_PREFIX"
    );
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.text_content().contains("EXPLICIT_STARTUP_FACT"))
    );
    assert!(
        !serde_json::to_string(&*requests)
            .unwrap()
            .contains("INERT_NOT_IN_PROMPT")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
struct Hidden(Arc<AtomicUsize>);
#[async_trait]
impl ExecutableTool for Hidden {
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        _: &ToolContext,
        _: serde_json::Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ToolFailure::new("unexpected", "hidden tool executed"))
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "hidden".into(),
            description: Some("fixture".into()),
            parameters: json!({"type":"object"}),
        }
    }
    async fn prepare(
        &self,
        _: &ToolContext,
        _: serde_json::Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ToolFailure::new(
            "unexpected",
            "hidden tool must not prepare",
        ))
    }
}
#[tokio::test]
async fn empty_startup_catalog_stays_empty_across_rounds_and_fresh_hidden_calls_never_prepare() {
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(Provider {
        call_hidden: true,
        ..Default::default()
    });
    let prepared = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry.register(Hidden(prepared.clone())).unwrap();
    let (h, calls) = hook(Mode::Empty);
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .registry(registry)
        .extensions(extensions(h))
        .build()
        .unwrap();
    let input = executor
        .prepare_input(
            ExecutionInput::from_request(request()).for_run(),
            &TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let report = executor.validate_model_selection(&input).unwrap();
    assert_eq!(report.spec.id, "m");
    let result = executor.run_input(input).await.unwrap();
    assert_eq!(result.rounds, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(prepared.load(Ordering::SeqCst), 0);
    assert!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.tools.is_empty())
    );
}
struct Ordered {
    first: bool,
}
#[async_trait]
impl BeforeStartHook for Ordered {
    async fn before_start(
        &self,
        input: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        if self.first {
            Ok(Some(BeforeStartAction {
                system_prefix: Some(vec![Message::text(MessageRole::System, "stage one")]),
                tools: Some(vec![]),
                append: vec![BoundaryEntry::Context {
                    kind: "one".into(),
                    text: "stage one fact".into(),
                }],
            }))
        } else {
            assert_eq!(input.request.messages[0].text_content(), "stage one");
            assert!(input.request.tools.is_empty());
            assert_eq!(input.entries.len(), 1);
            assert!(
                input
                    .history
                    .last()
                    .unwrap()
                    .text_content()
                    .contains("stage one fact")
            );
            assert_eq!(input.available_tools.len(), 1);
            Ok(Some(BeforeStartAction {
                system_prefix: Some(vec![Message::text(MessageRole::System, "stage two")]),
                tools: Some(vec!["hidden".into()]),
                ..Default::default()
            }))
        }
    }
}
#[tokio::test]
async fn startup_chains_observe_proposed_messages_and_may_reselect_only_original_root_definitions()
{
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(Provider::default());
    let mut e = Extension::new("ordered", "1").unwrap();
    let mut late = HookSpec::new("late");
    late.priority = 10;
    e.before_start_hook(late, Arc::new(Ordered { first: false }))
        .unwrap();
    e.before_start_hook(HookSpec::new("early"), Arc::new(Ordered { first: true }))
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let mut tools = ToolRegistry::default();
    tools
        .register(Hidden(Arc::new(AtomicUsize::new(0))))
        .unwrap();
    TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .registry(tools)
        .extensions(Arc::new(r.finish()))
        .build()
        .unwrap()
        .run(request())
        .await
        .unwrap();
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests[0].messages[0].text_content(), "stage two");
    assert_eq!(requests[0].tools[0].name, "hidden");
}
#[tokio::test]
async fn startup_invalid_prefix_payload_and_callback_failures_leave_session_unmodified_and_do_not_call_model()
 {
    for mode in [
        Mode::BadPrefix,
        Mode::Huge,
        Mode::Count,
        Mode::Fail,
        Mode::Panic,
    ] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let provider = Arc::new(Provider::default());
        let (h, calls) = hook(mode);
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(provider.clone())
                .workspace(root.path())
                .extensions(extensions(h))
                .build()
                .unwrap(),
            AgentConfig::new("m"),
        );
        let outcome = agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "raw input",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(provider.requests.lock().unwrap().is_empty());
        assert!(
            repo.open(&session.metadata().id)
                .unwrap()
                .entries()
                .is_empty()
        );
        let text = format!("{outcome:?}");
        assert!(!text.contains("PRIVATE_CALLBACK_ERROR"));
        assert!(!text.contains("PRIVATE_CALLBACK_PANIC"));
    }
}
struct UnknownTools;
#[async_trait]
impl BeforeStartHook for UnknownTools {
    async fn before_start(
        &self,
        _: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        Ok(Some(BeforeStartAction {
            tools: Some(vec!["outside".into()]),
            ..Default::default()
        }))
    }
}
#[tokio::test]
async fn unknown_root_tools_reject_before_provider_io() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let outcome = TurnExecutor::builder_from_shared(p.clone())
        .workspace(root.path())
        .extensions(extensions(Arc::new(UnknownTools)))
        .build()
        .unwrap()
        .run_outcome(request(), Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    assert!(p.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn output_budget_checks_complete_startup_before_any_session_mutation() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let p = Arc::new(Provider::default());
    let (h, _) = hook(Mode::Valid);
    let executor = TurnExecutor::builder_from_shared(p.clone())
        .workspace(root.path())
        .extensions(extensions(h))
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_output_bytes: Some(1),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let agent = Agent::new(executor, AgentConfig::new("m"));
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "raw input",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
    assert!(
        repo.open(&session.metadata().id)
            .unwrap()
            .entries()
            .is_empty()
    );
    assert!(p.requests.lock().unwrap().is_empty());
}
struct RequestAudit(Arc<AtomicUsize>);
#[async_trait]
impl RequestHook for RequestAudit {
    async fn request(
        &self,
        input: RequestHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<RequestEdit>> {
        assert_eq!(
            input.request.messages[0].text_content(),
            "PRIVATE_STARTUP_PREFIX"
        );
        assert!(
            input
                .history
                .iter()
                .any(|m| m.text_content().contains("EXPLICIT_STARTUP_FACT"))
        );
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}
#[tokio::test]
async fn startup_freezes_before_request_hooks_and_repeated_prepare_does_not_reexecute_it() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let (h, calls) = hook(Mode::Valid);
    let audits = Arc::new(AtomicUsize::new(0));
    let mut e = Extension::new("prepared", "1").unwrap();
    e.before_start_hook(HookSpec::new("start"), h).unwrap();
    e.request_hook(
        HookSpec::new("request"),
        RequestHookScope::Conversation,
        Arc::new(RequestAudit(audits.clone())),
    )
    .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let executor = TurnExecutor::builder_from_shared(p)
        .workspace(root.path())
        .extensions(Arc::new(r.finish()))
        .build()
        .unwrap();
    let input = executor
        .prepare_input(
            ExecutionInput::from_request(request()).for_run(),
            &TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let input = executor
        .prepare_input(input, &TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(audits.load(Ordering::SeqCst), 1);
    executor.run_input(input).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(audits.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn model_selection_preview_never_executes_startup_and_frozen_startup_rejects_another_executor()
 {
    let root = tempfile::tempdir().unwrap();
    let (h, calls) = hook(Mode::Valid);
    let executor = TurnExecutor::builder(Provider::default())
        .workspace(root.path())
        .extensions(extensions(h))
        .build()
        .unwrap();
    let input = executor
        .prepare_input(
            ExecutionInput::from_request(request()),
            &TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    executor.validate_model_selection(&input).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let input = executor
        .prepare_input(input.for_run(), &TurnExecutionContext::new())
        .await
        .unwrap();
    let other = TurnExecutor::builder(Provider::default())
        .workspace(root.path())
        .build()
        .unwrap();
    assert!(
        other
            .prepare_input(input.clone(), &TurnExecutionContext::new())
            .await
            .is_err()
    );
    assert!(matches!(
        other
            .run_input_outcome(input, Arc::new(()), TurnExecutionContext::new())
            .await,
        RunOutcome::Interrupted(_)
    ));
}
struct Journal {
    fail: bool,
    slow: bool,
    commits: AtomicUsize,
}
#[async_trait]
impl RunJournal for Journal {
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn record_lifecycle(&self, _: &LifecycleRecord) -> Result<()> {
        Ok(())
    }
    async fn commit_boundary(&self, _: &BoundaryCommit) -> Result<()> {
        self.commits.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            anyhow::bail!("commit refused")
        }
        if self.slow {
            std::thread::sleep(Duration::from_millis(150));
        }
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn failed_startup_commit_does_not_publish_notes_or_call_provider_and_acknowledged_notes_survive_deadline()
 {
    for slow in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let (h, _) = hook(Mode::Valid);
        let executor = TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(h))
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_duration: Some(Duration::from_millis(75)),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap();
        let journal = Journal {
            fail: !slow,
            slow,
            commits: AtomicUsize::new(0),
        };
        let outcome = executor
            .run_with_journal(
                request(),
                Arc::new(()),
                TurnExecutionContext::new(),
                &journal,
            )
            .await;
        assert_eq!(journal.commits.load(Ordering::SeqCst), 1);
        assert!(p.requests.lock().unwrap().is_empty());
        assert_eq!(
            outcome
                .progress()
                .messages
                .iter()
                .any(|m| m.text_content().contains("EXPLICIT_STARTUP_FACT")),
            slow
        );
        assert!(
            matches!(outcome,RunOutcome::Interrupted(ref i)if if slow{matches!(i.stop,RunStop::Budget(_))}else{matches!(i.stop,RunStop::Failed(_))})
        );
    }
}
struct Wait {
    started: Arc<tokio::sync::Notify>,
    ended: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl BeforeStartHook for Wait {
    async fn before_start(
        &self,
        _: BeforeStartInput<'_>,
        c: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        let ended = self.ended.clone();
        tokio::spawn(async move {
            c.cancelled().await;
            ended.notify_one();
        });
        self.started.notify_one();
        std::future::pending().await
    }
}
#[tokio::test]
async fn cancellation_or_hook_timeout_expires_private_children_before_model_or_session_write() {
    for cancel in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let id = session.metadata().id.clone();
        let p = Arc::new(Provider::default());
        let started = Arc::new(tokio::sync::Notify::new());
        let ended = Arc::new(tokio::sync::Notify::new());
        let mut e = Extension::new("wait", "1").unwrap();
        let mut spec = HookSpec::new("start");
        spec.timeout = Duration::from_millis(20);
        e.before_start_hook(
            spec,
            Arc::new(Wait {
                started: started.clone(),
                ended: ended.clone(),
            }),
        )
        .unwrap();
        let mut r = ExtensionRegistry::new();
        r.install(e).unwrap();
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(p.clone())
                .workspace(root.path())
                .extensions(Arc::new(r.finish()))
                .build()
                .unwrap(),
            AgentConfig::new("m"),
        );
        let context = TurnExecutionContext::new();
        let task_context = context.clone();
        let task_repo = repo.clone();
        let task_id = id.clone();
        let task = tokio::spawn(async move {
            agent
                .run_with_session_outcome(
                    &task_repo,
                    SessionSelection::Existing(task_id),
                    "raw",
                    Arc::new(()),
                    task_context,
                )
                .await
        });
        started.notified().await;
        if cancel {
            context.cancel("cancel startup");
        }
        let outcome = task.await.unwrap();
        assert!(matches!(
            outcome,
            AgentTurnOutcome::Aborted(_) | AgentTurnOutcome::Failed(_)
        ));
        tokio::time::timeout(Duration::from_secs(1), ended.notified())
            .await
            .unwrap();
        assert!(repo.open(&id).unwrap().entries().is_empty());
        assert!(p.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn older_boundary_receipts_default_optional_startup_without_changing_their_wire_shape() {
    let value = json!({"kind":"turn_end","round":1,"base_index":2,"entries":[],"continue_run":false,"changes":[]});
    let restored: BoundaryCommit = serde_json::from_value(value.clone()).unwrap();
    assert!(restored.startup.is_none());
    assert_eq!(serde_json::to_value(restored).unwrap(), value);
}
struct Unsupported;
#[async_trait]
impl RunJournal for Unsupported {
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn unsupported_journal_rejects_startup_before_callbacks_or_paid_model_io() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let (h, calls) = hook(Mode::Valid);
    let executor = TurnExecutor::builder_from_shared(p.clone())
        .workspace(root.path())
        .extensions(extensions(h))
        .build()
        .unwrap();
    let outcome = executor
        .run_with_journal(
            request(),
            Arc::new(()),
            TurnExecutionContext::new(),
            &Unsupported,
        )
        .await;
    assert!(matches!(outcome,RunOutcome::Interrupted(ref i)if matches!(i.stop,RunStop::Failed(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(p.requests.lock().unwrap().is_empty());
}
struct Pause {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl BeforeStartHook for Pause {
    async fn before_start(
        &self,
        _: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.release.notified().await;
        Ok(Some(BeforeStartAction {
            append: vec![BoundaryEntry::Context {
                kind: "queued".into(),
                text: "STARTUP_QUEUE_FACT".into(),
            }],
            ..Default::default()
        }))
    }
}
#[tokio::test]
async fn steering_queued_while_startup_waits_joins_the_owned_run_without_restarting_startup() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(Arc::new(Pause {
                started: started.clone(),
                release: release.clone(),
                calls: calls.clone(),
            })))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let (handle, control) = RunHandle::pair();
    let run = tokio::spawn(async move {
        agent
            .run_with_session_controlled(
                &repo,
                SessionSelection::Existing(id),
                "original",
                Arc::new(()),
                control,
            )
            .await
    });
    started.notified().await;
    handle
        .steer(Message::text(MessageRole::User, "queued during startup"))
        .unwrap();
    release.notify_one();
    let outcome = run.await.unwrap();
    assert!(matches!(outcome, AgentTurnOutcome::Completed(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let requests = p.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.text_content() == "queued during startup")
    );
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.text_content().contains("STARTUP_QUEUE_FACT"))
    );
    assert!(handle.snapshot().pending_inputs.is_empty());
}

struct FirstRunOnly(AtomicUsize);
#[async_trait]
impl BeforeStartHook for FirstRunOnly {
    async fn before_start(
        &self,
        _: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        Ok(
            (self.0.fetch_add(1, Ordering::SeqCst) == 0).then(|| BeforeStartAction {
                system_prefix: Some(vec![Message::text(MessageRole::System, "first run only")]),
                ..Default::default()
            }),
        )
    }
}
#[tokio::test]
async fn prefix_override_expires_between_turns_on_the_same_live_agent() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let provider = Arc::new(Provider::default());
    let callback = Arc::new(FirstRunOnly(AtomicUsize::new(0)));
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(extensions(callback.clone()))
            .build()
            .unwrap(),
        AgentConfig::new("m").system_prompt("base host prefix"),
    );
    for text in ["first", "second"] {
        agent
            .run_with_session(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                text,
                Arc::new(()),
            )
            .await
            .unwrap();
    }
    assert_eq!(callback.0.load(Ordering::SeqCst), 2);
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests[0].messages[0].text_content(), "first run only");
    assert_eq!(requests[1].messages[0].text_content(), "base host prefix");
    assert!(
        !serde_json::to_string(repo.open(&session.metadata().id).unwrap().entries())
            .unwrap()
            .contains("first run only")
    );
}
