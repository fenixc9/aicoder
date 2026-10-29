use aicoder_core::{
    model::*,
    session::{MemorySessionRepository, SessionRepository},
    tools::*,
    *,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
struct Script(Mutex<VecDeque<ModelResponse>>);
impl Script {
    fn new(names: &[&str]) -> Self {
        let mut steps = VecDeque::new();
        for (index, name) in names.iter().enumerate() {
            steps.push_back(ModelResponse {
                id: None,
                model: "offline".into(),
                message: Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: format!("call-{index}"),
                            name: (*name).into(),
                            arguments: json!({"operation":index}),
                        },
                    }],
                },
                stop_reason: StopReason::ToolCalls,
                usage: ModelUsage::default(),
            });
            steps.push_back(ModelResponse {
                id: None,
                model: "offline".into(),
                message: Message::text(MessageRole::Assistant, "done"),
                stop_reason: StopReason::Complete,
                usage: ModelUsage::default(),
            });
        }
        Self(Mutex::new(steps))
    }
}
#[async_trait]
impl ModelProvider for Script {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        _: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let response = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted");
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Tool {
    name: &'static str,
    executed: Arc<AtomicUsize>,
    fail_prepare: Arc<AtomicBool>,
    command: Arc<AtomicBool>,
}
#[async_trait]
impl ExecutableTool for Tool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        if self.command.load(Ordering::SeqCst) {
            ToolCapability::Command
        } else {
            ToolCapability::Mutating
        }
    }
    async fn prepare(
        &self,
        _: &ToolContext,
        _: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        if self.fail_prepare.load(Ordering::SeqCst) {
            return Err(ToolFailure::new("precondition_failed", "test precondition"));
        }
        Ok(None)
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        self.executed.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess::new(json!({"executed":true})))
    }
}
fn registry(
    names: &[&'static str],
    executed: &Arc<AtomicUsize>,
    fail: &Arc<AtomicBool>,
    command: &Arc<AtomicBool>,
) -> Arc<ToolRegistry> {
    let mut registry = ToolRegistry::default();
    for name in names {
        registry
            .register(Tool {
                name,
                executed: executed.clone(),
                fail_prepare: fail.clone(),
                command: command.clone(),
            })
            .unwrap();
    }
    Arc::new(registry)
}
#[derive(Clone)]
struct Decide {
    decision: ApprovalDecision,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl ApprovalHandler for Decide {
    async fn decide(&self, request: &ApprovalRequest<'_>) -> anyhow::Result<ApprovalDecision> {
        assert!(!request.invocation.name.is_empty());
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.decision)
    }
}
fn executor(
    root: &Path,
    names: &[&str],
    registry: Arc<ToolRegistry>,
    handler: impl ApprovalHandler + 'static,
) -> TurnExecutor {
    TurnExecutor::builder(Script::new(names))
        .workspace(root)
        .shared_registry(registry)
        .approval(handler)
        .build()
        .unwrap()
}
async fn run(executor: &TurnExecutor, session: Option<ApprovalSession>) -> TurnExecutionResult {
    let context = match session {
        Some(session) => TurnExecutionContext::new().with_approval_session(session),
        None => TurnExecutionContext::new(),
    };
    executor
        .run_with_context(
            ModelRequest::new("offline", vec![Message::text(MessageRole::User, "work")]),
            Arc::new(()),
            context,
        )
        .await
        .unwrap()
}
fn output(result: &TurnExecutionResult) -> &Value {
    result
        .messages
        .iter()
        .rev()
        .find_map(Message::tool_result)
        .unwrap()
        .1
}
fn setup() -> (Arc<AtomicUsize>, Arc<AtomicBool>, Arc<AtomicBool>) {
    (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    )
}
#[tokio::test]
async fn deny_once_and_session_choices_have_distinct_cross_turn_lifetimes() {
    for decision in [
        ApprovalDecision::Deny,
        ApprovalDecision::AllowOnce,
        ApprovalDecision::AllowSession,
    ] {
        let root = tempfile::tempdir().unwrap();
        let cache = ApprovalSessions::default();
        let repository = MemorySessionRepository::new();
        let (executed, fail, command) = setup();
        let calls = Arc::new(AtomicUsize::new(0));
        let executor = executor(
            root.path(),
            &["write", "write", "write"],
            registry(&["write"], &executed, &fail, &command),
            Decide {
                decision,
                calls: calls.clone(),
            },
        );
        let agent = Agent::new(executor, AgentConfig::new("offline"));
        let first = agent
            .run_with_session_context(
                &repository,
                SessionSelection::New,
                "one",
                Arc::new(()),
                TurnExecutionContext::new().with_approval_sessions(cache.clone()),
            )
            .await
            .unwrap();
        let id = first.session.unwrap().id;
        agent
            .run_with_session_context(
                &repository,
                SessionSelection::Existing(id.clone()),
                "two",
                Arc::new(()),
                TurnExecutionContext::new().with_approval_sessions(cache.clone()),
            )
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if decision == ApprovalDecision::AllowSession {
                1
            } else {
                2
            }
        );
        assert_eq!(
            executed.load(Ordering::SeqCst),
            if decision == ApprovalDecision::Deny {
                0
            } else {
                2
            }
        );
        let session = cache.get(root.path(), &id).unwrap();
        let snapshot = session.snapshot();
        assert_eq!(
            snapshot.grants.len(),
            usize::from(decision == ApprovalDecision::AllowSession)
        );
        if let Some(grant) = snapshot.grants.first() {
            assert!(session.revoke(&grant.id));
            assert!(!session.revoke(&grant.id));
        }
        agent
            .run_with_session_context(
                &repository,
                SessionSelection::Existing(id),
                "three",
                Arc::new(()),
                TurnExecutionContext::new().with_approval_sessions(cache.clone()),
            )
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if decision == ApprovalDecision::AllowSession {
                2
            } else {
                3
            }
        );
    }
}
#[tokio::test]
async fn grants_are_isolated_by_host_workspace_session_tool_registry_and_capability() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let cache = ApprovalSessions::default();
    let scope = cache.session(a.path(), "chat").unwrap();
    let (executed, fail, command) = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let policy = Decide {
        decision: ApprovalDecision::AllowSession,
        calls: calls.clone(),
    };
    let shared = registry(&["write", "other"], &executed, &fail, &command);
    let exec = executor(
        a.path(),
        &["write", "write", "other", "write", "write", "write"],
        shared.clone(),
        policy.clone(),
    );
    run(&exec, Some(scope.clone())).await;
    run(&exec, Some(scope.clone())).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    run(&exec, Some(scope.clone())).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    run(&exec, Some(cache.session(a.path(), "other-chat").unwrap())).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    run(
        &exec,
        Some(
            ApprovalSessions::default()
                .session(a.path(), "chat")
                .unwrap(),
        ),
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    command.store(true, Ordering::SeqCst);
    run(&exec, Some(scope.clone())).await;
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    let other_root = executor(b.path(), &["write", "write"], shared, policy.clone());
    let wrong = run(&other_root, Some(scope)).await;
    assert_eq!(output(&wrong)["error"]["code"], "approval_scope_mismatch");
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    run(&other_root, Some(cache.session(b.path(), "chat").unwrap())).await;
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    let fresh_registry = executor(
        a.path(),
        &["write"],
        registry(&["write"], &executed, &fail, &command),
        policy,
    );
    run(
        &fresh_registry,
        Some(cache.session(a.path(), "chat").unwrap()),
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 7);
}
#[tokio::test]
async fn cached_grants_never_skip_tool_preparation() {
    let root = tempfile::tempdir().unwrap();
    let cache = ApprovalSessions::default();
    let scope = cache.session(root.path(), "chat").unwrap();
    let (executed, fail, command) = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let exec = executor(
        root.path(),
        &["write", "write"],
        registry(&["write"], &executed, &fail, &command),
        Decide {
            decision: ApprovalDecision::AllowSession,
            calls: calls.clone(),
        },
    );
    run(&exec, Some(scope.clone())).await;
    fail.store(true, Ordering::SeqCst);
    let second = run(&exec, Some(scope)).await;
    assert_eq!(output(&second)["error"]["code"], "precondition_failed");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(executed.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn limits_eviction_clear_and_serialized_snapshots_cannot_retain_unbounded_authority() {
    let root = tempfile::tempdir().unwrap();
    let cache = ApprovalSessions::with_limits(ApprovalCacheLimits {
        max_sessions: 1,
        max_grants_per_session: 1,
    })
    .unwrap();
    let scope = cache.session(root.path(), "a").unwrap();
    let (executed, fail, command) = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let exec = executor(
        root.path(),
        &["write", "other", "write"],
        registry(&["write", "other"], &executed, &fail, &command),
        Decide {
            decision: ApprovalDecision::AllowSession,
            calls: calls.clone(),
        },
    );
    run(&exec, Some(scope.clone())).await;
    let data = serde_json::to_string(&scope.snapshot()).unwrap();
    let _: ApprovalSnapshot = serde_json::from_str(&data).unwrap();
    let error = run(&exec, Some(scope.clone())).await;
    assert_eq!(output(&error)["error"]["code"], "approval_limit");
    assert_eq!(executed.load(Ordering::SeqCst), 1);
    let _b = cache.session(root.path(), "b").unwrap();
    assert!(scope.snapshot().session.closed);
    assert!(scope.snapshot().grants.is_empty());
    assert!(cache.get(root.path(), "a").is_none());
    let old = run(&exec, Some(scope.clone())).await;
    assert_eq!(output(&old)["error"]["code"], "approval_revoked");
    let fresh = cache.session(root.path(), "a").unwrap();
    assert_ne!(
        scope.snapshot().session.scope_id,
        fresh.snapshot().session.scope_id
    );
    assert!(fresh.snapshot().grants.is_empty());
    cache.clear();
    assert!(fresh.snapshot().session.closed);
    assert!(
        ApprovalSessions::with_limits(ApprovalCacheLimits {
            max_sessions: 0,
            max_grants_per_session: 1
        })
        .is_err()
    );
    assert!(cache.session(root.path(), "").is_err());
}
async fn pending(handle: &RunHandle) -> PendingRunApproval {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = handle.snapshot();
            if let Some(request) = snapshot.approvals.first() {
                return request.clone();
            }
            handle.changed(snapshot.revision).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn revoked_cancelled_and_foreign_typed_responses_cannot_install_grants() {
    for cancel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let cache = ApprovalSessions::default();
        let scope = cache.session(root.path(), "chat").unwrap();
        let (executed, fail, command) = setup();
        let exec = executor(
            root.path(),
            &["write"],
            registry(&["write"], &executed, &fail, &command),
            DenyAllApproval,
        );
        let (handle, control) = RunHandle::channel(
            TurnExecutionContext::new().with_approval_session(scope.clone()),
            RunControlConfig {
                handle_approvals: true,
                ..Default::default()
            },
        )
        .unwrap();
        let worker = tokio::spawn(async move {
            exec.run_controlled(
                ModelRequest::new("offline", vec![Message::text(MessageRole::User, "work")]),
                Arc::new(()),
                control,
            )
            .await
        });
        let request = pending(&handle).await;
        assert!(request.session.is_some());
        let foreign = ApprovalRequestId {
            run_id: uuid::Uuid::new_v4().to_string(),
            sequence: request.id.sequence,
        };
        assert_eq!(
            handle.respond_approval_decision(&foreign, ApprovalDecision::AllowSession),
            Err(RunControlError::UnknownApproval)
        );
        if cancel {
            handle.cancel("cancel pending session grant").unwrap();
            assert_eq!(
                handle.respond_approval_decision(&request.id, ApprovalDecision::AllowSession),
                Err(RunControlError::Closed)
            );
        } else {
            scope.revoke_all();
            handle
                .respond_approval_decision(&request.id, ApprovalDecision::AllowSession)
                .unwrap();
        }
        let outcome = worker.await.unwrap();
        if cancel {
            assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        } else {
            let RunOutcome::Completed(result) = outcome else {
                panic!("expected error result recovery")
            };
            assert_eq!(output(&result)["error"]["code"], "approval_revoked");
        }
        assert!(scope.snapshot().grants.is_empty());
        assert_eq!(executed.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn session_decisions_require_a_scope_and_invalid_responses_leave_the_request_open() {
    let root = tempfile::tempdir().unwrap();
    let (executed, fail, command) = setup();
    let exec = executor(
        root.path(),
        &["write"],
        registry(&["write"], &executed, &fail, &command),
        DenyAllApproval,
    );
    let (handle, control) = RunHandle::channel(
        TurnExecutionContext::new(),
        RunControlConfig {
            handle_approvals: true,
            ..Default::default()
        },
    )
    .unwrap();
    let worker = tokio::spawn(async move {
        exec.run_controlled(
            ModelRequest::new("offline", vec![Message::text(MessageRole::User, "work")]),
            Arc::new(()),
            control,
        )
        .await
    });
    let request = pending(&handle).await;
    assert!(request.session.is_none());
    assert_eq!(
        handle.respond_approval_decision(&request.id, ApprovalDecision::AllowSession),
        Err(RunControlError::InvalidApprovalDecision)
    );
    assert_eq!(handle.snapshot().approvals.len(), 1);
    handle.respond_approval(&request.id, true).unwrap();
    assert!(matches!(worker.await.unwrap(), RunOutcome::Completed(_)));
    assert_eq!(executed.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn new_host_caches_do_not_restore_grants_from_existing_session_history() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let cache = ApprovalSessions::default();
    let (executed, fail, command) = setup();
    let shared = registry(&["write"], &executed, &fail, &command);
    let agent = Agent::new(
        executor(
            root.path(),
            &["write"],
            shared.clone(),
            Decide {
                decision: ApprovalDecision::AllowSession,
                calls: Arc::new(AtomicUsize::new(0)),
            },
        ),
        AgentConfig::new("offline"),
    );
    let first = agent
        .run_with_session_context(
            &repo,
            SessionSelection::New,
            "grant",
            Arc::new(()),
            TurnExecutionContext::new().with_approval_sessions(cache.clone()),
        )
        .await
        .unwrap();
    let id = first.session.unwrap().id;
    assert_eq!(
        cache.get(root.path(), &id).unwrap().snapshot().grants.len(),
        1
    );
    let restarted = ApprovalSessions::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        executor(
            root.path(),
            &["write"],
            shared,
            Decide {
                decision: ApprovalDecision::Deny,
                calls: calls.clone(),
            },
        ),
        AgentConfig::new("offline"),
    );
    agent
        .run_with_session_context(
            &repo,
            SessionSelection::Existing(id.clone()),
            "resume",
            Arc::new(()),
            TurnExecutionContext::new().with_approval_sessions(restarted.clone()),
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(executed.load(Ordering::SeqCst), 1);
    assert!(
        restarted
            .get(root.path(), &id)
            .unwrap()
            .snapshot()
            .grants
            .is_empty()
    );
    assert!(!repo.open(&id).unwrap().entries().is_empty());
}
#[tokio::test]
async fn a_mismatched_explicit_scope_fails_before_session_history_is_changed() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let scope = ApprovalSessions::default()
        .session(root.path(), "foreign")
        .unwrap();
    let (executed, fail, command) = setup();
    let agent = Agent::new(
        executor(
            root.path(),
            &[],
            registry(&["write"], &executed, &fail, &command),
            AllowAllApproval,
        ),
        AgentConfig::new("offline"),
    );
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            "work",
            Arc::new(()),
            TurnExecutionContext::new().with_approval_session(scope),
        )
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
    assert_eq!(repo.open(&id).unwrap().metadata().revision, 0);
}

struct CancelInsideDecision(TurnExecutionContext);
#[async_trait]
impl ApprovalHandler for CancelInsideDecision {
    async fn decide(&self, _: &ApprovalRequest<'_>) -> anyhow::Result<ApprovalDecision> {
        self.0.cancel("cancel before grant commit");
        Ok(ApprovalDecision::AllowSession)
    }
}
#[tokio::test]
async fn cancellation_inside_a_ready_handler_cannot_commit_a_late_grant() {
    let root = tempfile::tempdir().unwrap();
    let scope = ApprovalSessions::default()
        .session(root.path(), "chat")
        .unwrap();
    let context = TurnExecutionContext::new().with_approval_session(scope.clone());
    let (executed, fail, command) = setup();
    let exec = executor(
        root.path(),
        &["write"],
        registry(&["write"], &executed, &fail, &command),
        CancelInsideDecision(context.clone()),
    );
    let outcome = exec
        .run_outcome(
            ModelRequest::new("offline", vec![Message::text(MessageRole::User, "work")]),
            Arc::new(()),
            context,
        )
        .await;
    let RunOutcome::Interrupted(interrupted) = outcome else {
        panic!("expected cancellation")
    };
    assert!(matches!(interrupted.stop, RunStop::Cancelled { .. }));
    assert!(scope.snapshot().grants.is_empty());
    assert_eq!(executed.load(Ordering::SeqCst), 0);
}
#[cfg(unix)]
#[tokio::test]
async fn replacing_a_workspace_directory_does_not_reuse_its_old_grants() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    let cache = ApprovalSessions::default();
    let scope = cache.session(&root, "chat").unwrap();
    let (executed, fail, command) = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let exec = executor(
        &root,
        &["write", "write", "write"],
        registry(&["write"], &executed, &fail, &command),
        Decide {
            decision: ApprovalDecision::AllowSession,
            calls: calls.clone(),
        },
    );
    run(&exec, Some(scope.clone())).await;
    std::fs::rename(&root, parent.path().join("old")).unwrap();
    std::fs::create_dir(&root).unwrap();
    let old = run(&exec, Some(scope)).await;
    assert_eq!(output(&old)["error"]["code"], "approval_scope_mismatch");
    let fresh = cache.session(&root, "chat").unwrap();
    assert!(fresh.snapshot().grants.is_empty());
    run(&exec, Some(fresh)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(executed.load(Ordering::SeqCst), 2);
}

struct SteerInsideDecision(RunHandle);
#[async_trait]
impl ApprovalHandler for SteerInsideDecision {
    async fn decide(&self, _: &ApprovalRequest<'_>) -> anyhow::Result<ApprovalDecision> {
        self.0
            .steer(Message::text(MessageRole::User, "redirect"))
            .unwrap();
        Ok(ApprovalDecision::AllowSession)
    }
}
#[tokio::test]
async fn steering_inside_a_ready_handler_does_not_install_an_unused_grant() {
    let root = tempfile::tempdir().unwrap();
    let scope = ApprovalSessions::default()
        .session(root.path(), "chat")
        .unwrap();
    let (handle, control) = RunHandle::channel(
        TurnExecutionContext::new().with_approval_session(scope.clone()),
        Default::default(),
    )
    .unwrap();
    let (executed, fail, command) = setup();
    let exec = executor(
        root.path(),
        &["write"],
        registry(&["write"], &executed, &fail, &command),
        SteerInsideDecision(handle),
    );
    let outcome = exec
        .run_controlled(
            ModelRequest::new("offline", vec![Message::text(MessageRole::User, "work")]),
            Arc::new(()),
            control,
        )
        .await;
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert!(scope.snapshot().grants.is_empty());
    assert_eq!(executed.load(Ordering::SeqCst), 0);
}

#[test]
fn published_session_grant_trace_validates_but_contradictory_authorization_metadata_does_not() {
    let frames = include_str!("../../../docs/protocol/session-approvals-v1-trace.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<AgentRawEventEnvelope>(line).unwrap())
        .collect::<Vec<_>>();
    let mut current = None;
    let mut validator = events::EventTraceValidator::new();
    for frame in &frames {
        if current.is_some_and(|run| run != frame.run_id) {
            validator.finish().unwrap();
            validator = events::EventTraceValidator::new();
        }
        current = Some(frame.run_id);
        validator.observe(frame).unwrap();
    }
    validator.finish().unwrap();
    let first = frames
        .iter()
        .take_while(|frame| frame.run_id == frames[0].run_id)
        .cloned()
        .collect::<Vec<_>>();
    for missing_grant in [false, true] {
        let mut altered = first.clone();
        let frame = altered
            .iter_mut()
            .find(|frame| {
                matches!(
                    frame.event,
                    AgentRawEvent::ToolApprovalResolved {
                        resolution: Some(_),
                        ..
                    }
                )
            })
            .unwrap();
        if let AgentRawEvent::ToolApprovalResolved {
            resolution: Some(resolution),
            ..
        } = &mut frame.event
        {
            let resolution = Arc::make_mut(resolution);
            if missing_grant {
                resolution.grant_id = None;
            } else {
                resolution.decision = ApprovalDecision::Deny;
            }
        }
        let mut validator = events::EventTraceValidator::new();
        assert!(
            altered
                .iter()
                .any(|frame| validator.observe(frame).is_err())
        );
        assert!(validator.finish().is_err());
    }
}

#[tokio::test]
async fn revoking_an_open_question_expires_it_without_waiting_for_a_human_response() {
    let root = tempfile::tempdir().unwrap();
    let cache = ApprovalSessions::default();
    let scope = cache.session(root.path(), "chat").unwrap();
    let (executed, fail, command) = setup();
    let exec = executor(
        root.path(),
        &["write"],
        registry(&["write"], &executed, &fail, &command),
        DenyAllApproval,
    );
    let (handle, control) = RunHandle::channel(
        TurnExecutionContext::new().with_approval_session(scope.clone()),
        RunControlConfig {
            handle_approvals: true,
            ..Default::default()
        },
    )
    .unwrap();
    let worker = tokio::spawn(async move {
        exec.run_controlled(
            ModelRequest::new("offline", vec![Message::text(MessageRole::User, "work")]),
            Arc::new(()),
            control,
        )
        .await
    });
    let request = pending(&handle).await;
    scope.revoke_all();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap();
    let RunOutcome::Completed(result) = outcome else {
        panic!("expected permission failure recovery")
    };
    assert_eq!(output(&result)["error"]["code"], "approval_revoked");
    assert!(handle.snapshot().approvals.is_empty());
    assert!(
        handle
            .respond_approval_decision(&request.id, ApprovalDecision::AllowSession)
            .is_err()
    );
    assert_eq!(executed.load(Ordering::SeqCst), 0);
    assert!(scope.snapshot().grants.is_empty());
}
