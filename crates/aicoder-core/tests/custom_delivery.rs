use aicoder_core::{events::*, extensions::*, model::*, session::*, *};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
#[derive(Default)]
struct Provider {
    requests: Mutex<Vec<ModelRequest>>,
    started: Notify,
    release: Notify,
    gate: bool,
    images: bool,
    window: Option<u64>,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", model);
        s.capabilities.images = self.images;
        s.context_tokens = Some(self.window.unwrap_or(16384));
        Ok(s)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let first = self.requests.lock().unwrap().is_empty();
        self.requests.lock().unwrap().push(request.clone());
        if first && self.gate {
            self.started.notify_one();
            self.release.notified().await;
        }
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "answer"),
            stop_reason: StopReason::Complete,
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
async fn wait(n: &Notify) {
    tokio::time::timeout(Duration::from_secs(2), n.notified())
        .await
        .unwrap();
}
fn custom(text: &str) -> CustomMessage {
    CustomMessage::text("sender", "note", text)
        .unwrap()
        .with_details(json!({"inert":"DETAILS_NEVER_PROMPTED"}))
        .unwrap()
}
fn agent(root: &std::path::Path, p: Arc<Provider>) -> Arc<Agent> {
    Arc::new(Agent::new(
        TurnExecutor::builder_from_shared(p)
            .workspace(root)
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    ))
}
#[tokio::test]
async fn custom_steer_followup_and_deferred_context_preserve_real_history_and_requested_continuation()
 {
    for delivery in [
        CustomDelivery::Steer,
        CustomDelivery::FollowUp,
        CustomDelivery::TurnEnd,
    ] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let id = session.metadata().id.clone();
        let p = Arc::new(Provider {
            gate: true,
            ..Default::default()
        });
        let agent = agent(root.path(), p.clone());
        let (handle, control) = agent
            .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
            .unwrap();
        let owner = agent.clone();
        let task_repo = repo.clone();
        let task_id = id.clone();
        let run = tokio::spawn(async move {
            owner
                .run_with_session_controlled(
                    &task_repo,
                    SessionSelection::Existing(task_id),
                    "original",
                    Arc::new(()),
                    control,
                )
                .await
        });
        wait(&p.started).await;
        let result = handle
            .submit_custom(delivery, custom("CUSTOM_ACTIVE_FACT"))
            .await
            .unwrap();
        assert!(matches!(result, CustomSubmission::Queued { .. }));
        p.release.notify_one();
        let outcome = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap();
        let result = outcome.into_result().unwrap();
        let expected = if delivery == CustomDelivery::TurnEnd {
            1
        } else {
            2
        };
        assert_eq!(p.requests.lock().unwrap().len(), expected);
        assert_eq!(
            result.execution_result.usage.total_tokens,
            Some(expected as u64 * 7)
        );
        assert_eq!(
            result.execution_result.final_message.text_content(),
            "answer"
        );
        let saved = repo.open(&id).unwrap();
        let messages = saved.messages();
        assert_eq!(
            messages
                .iter()
                .filter(|m| m.message.role == MessageRole::User)
                .count(),
            1
        );
        assert_eq!(
            messages
                .iter()
                .filter(|m| m.message.role == MessageRole::Custom)
                .count(),
            1
        );
        assert_eq!(messages[1].message.role, MessageRole::Assistant);
        assert_eq!(messages[2].message.role, MessageRole::Custom);
        let requests = p.requests.lock().unwrap();
        if expected == 2 {
            assert!(
                requests[1]
                    .messages
                    .iter()
                    .any(|m| m.text_content().contains("CUSTOM_ACTIVE_FACT"))
            );
        }
        assert!(
            !serde_json::to_string(&*requests)
                .unwrap()
                .contains("DETAILS_NEVER_PROMPTED")
        );
    }
}
#[tokio::test]
async fn next_turn_custom_context_is_inert_until_a_new_explicit_session_run_and_consumed_once() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider {
        gate: true,
        ..Default::default()
    });
    let agent = agent(root.path(), p.clone());
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let owner = agent.clone();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let run = tokio::spawn(async move {
        owner
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&p.started).await;
    assert!(matches!(
        handle
            .submit_custom(CustomDelivery::NextTurn, custom("NEXT_TURN_FACT"))
            .await
            .unwrap(),
        CustomSubmission::NextTurn { .. }
    ));
    assert_eq!(handle.next_turn_custom_messages().len(), 1);
    p.release.notify_one();
    run.await.unwrap().into_result().unwrap();
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert!(
        !serde_json::to_string(repo.open(&id).unwrap().entries())
            .unwrap()
            .contains("NEXT_TURN_FACT")
    );
    assert_eq!(agent.custom_inbox().pending(&id).len(), 1);
    agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(id.clone()),
            "second",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert!(agent.custom_inbox().pending(&id).is_empty());
    let requests = p.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        !serde_json::to_string(&requests[0])
            .unwrap()
            .contains("NEXT_TURN_FACT")
    );
    assert!(
        serde_json::to_string(&requests[1])
            .unwrap()
            .contains("NEXT_TURN_FACT")
    );
    drop(requests);
    let saved = repo.open(&id).unwrap();
    assert_eq!(
        saved
            .messages()
            .iter()
            .filter(|m| m.message.role == MessageRole::Custom)
            .count(),
        1
    );
    assert_eq!(
        saved
            .messages()
            .iter()
            .filter(|m| m.message.role == MessageRole::User)
            .count(),
        2
    );
}
struct FailStartup(Arc<AtomicBool>);
#[async_trait]
impl BeforeStartHook for FailStartup {
    async fn before_start(
        &self,
        _: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        anyhow::ensure!(!self.0.load(Ordering::SeqCst), "startup refused");
        Ok(None)
    }
}
#[tokio::test]
async fn failed_next_turn_preparation_releases_claim_and_retains_queued_data_for_retry() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider {
        gate: true,
        ..Default::default()
    });
    let fail = Arc::new(AtomicBool::new(false));
    let mut e = Extension::new("gate", "1").unwrap();
    e.before_start_hook(HookSpec::new("start"), Arc::new(FailStartup(fail.clone())))
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let agent = Arc::new(Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    ));
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let owner = agent.clone();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let run = tokio::spawn(async move {
        owner
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&p.started).await;
    handle
        .submit_custom(CustomDelivery::NextTurn, custom("CLAIMED_CONTEXT"))
        .await
        .unwrap();
    p.release.notify_one();
    run.await.unwrap().into_result().unwrap();
    let before = repo.open(&id).unwrap().entries().len();
    fail.store(true, Ordering::SeqCst);
    let failed = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            "second",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(failed, AgentTurnOutcome::Failed(_)));
    assert_eq!(repo.open(&id).unwrap().entries().len(), before);
    assert_eq!(agent.custom_inbox().pending(&id).len(), 1);
    fail.store(false, Ordering::SeqCst);
    agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(id.clone()),
            "retry",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert!(agent.custom_inbox().pending(&id).is_empty());
    assert_eq!(p.requests.lock().unwrap().len(), 2);
}
struct Settle {
    started: Arc<Notify>,
    release: Arc<Notify>,
}
#[async_trait]
impl BoundaryHook for Settle {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        if input.kind == BoundaryKind::BeforeSettle {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(None)
    }
}
#[tokio::test]
async fn context_accepted_during_settlement_is_journaled_without_an_extra_request() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider::default());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut e = Extension::new("settle", "1").unwrap();
    e.boundary_hook(
        HookSpec::new("wait"),
        Arc::new(Settle {
            started: started.clone(),
            release: release.clone(),
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
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let run = tokio::spawn(async move {
        agent
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&started).await;
    handle
        .submit_custom(CustomDelivery::TurnEnd, custom("LATE_CONTEXT"))
        .await
        .unwrap();
    release.notify_one();
    run.await.unwrap().into_result().unwrap();
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert!(
        repo.open(&id)
            .unwrap()
            .messages()
            .last()
            .unwrap()
            .message
            .text_content()
            .contains("LATE_CONTEXT")
    );
}
#[tokio::test]
async fn cancellation_before_a_boundary_retains_unapplied_custom_payload_without_activating_history()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider {
        gate: true,
        ..Default::default()
    });
    let agent = agent(root.path(), p.clone());
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let run = tokio::spawn(async move {
        agent
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&p.started).await;
    handle
        .submit_custom(CustomDelivery::TurnEnd, custom("UNAPPLIED_FACT"))
        .await
        .unwrap();
    handle.cancel("stop").unwrap();
    let outcome = run.await.unwrap();
    assert!(matches!(outcome, AgentTurnOutcome::Aborted(_)));
    assert_eq!(outcome.progress().pending_inputs.len(), 1);
    assert!(
        outcome.progress().pending_inputs[0]
            .custom_message()
            .unwrap()
            .unwrap()
            .message()
            .text_content()
            .contains("UNAPPLIED_FACT")
    );
    assert!(
        repo.open(&id)
            .unwrap()
            .messages()
            .iter()
            .all(|m| m.message.role != MessageRole::Custom)
    );
}
#[tokio::test]
async fn next_turn_inbox_is_session_scoped_bounded_discardable_and_not_restored_from_history() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider {
        gate: true,
        ..Default::default()
    });
    let agent = agent(root.path(), p.clone());
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let owner = agent.clone();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let run = tokio::spawn(async move {
        owner
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&p.started).await;
    let mut first = None;
    for i in 0..32 {
        let CustomSubmission::NextTurn { queue_id } = handle
            .submit_custom(CustomDelivery::NextTurn, custom(&format!("NEXT_{i}")))
            .await
            .unwrap()
        else {
            panic!()
        };
        first.get_or_insert(queue_id);
    }
    let error = handle
        .submit_custom(CustomDelivery::NextTurn, custom("overflow"))
        .await
        .unwrap_err();
    assert_eq!(
        error.error.downcast_ref::<RunControlError>(),
        Some(&RunControlError::QueueFull)
    );
    agent.custom_inbox().discard(&id, first.unwrap()).unwrap();
    assert_eq!(agent.custom_inbox().pending(&id).len(), 31);
    p.release.notify_one();
    run.await.unwrap().into_result().unwrap();
    let other = repo.create(root.path()).unwrap();
    agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(other.metadata().id.clone()),
            "other",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert_eq!(agent.custom_inbox().pending(&id).len(), 31);
    assert!(
        !serde_json::to_string(p.requests.lock().unwrap().last().unwrap())
            .unwrap()
            .contains("NEXT_")
    );
    let fresh = Agent::new(
        TurnExecutor::builder(Provider::default())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    assert!(fresh.custom_inbox().pending(&id).is_empty());
    assert_eq!(
        agent
            .with_reasoning_effort(None)
            .custom_inbox()
            .pending(&id)
            .len(),
        31
    );
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
async fn unsupported_journals_reject_custom_before_provider_io_and_user_context_kind_cannot_bypass_preparation()
 {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let executor = TurnExecutor::builder_from_shared(p.clone())
        .workspace(root.path())
        .build()
        .unwrap();
    let (handle, control) = executor
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    handle
        .submit_custom(CustomDelivery::Steer, custom("must not lose"))
        .await
        .unwrap();
    let outcome = executor
        .run_controlled_with_journal(
            ModelRequest::new("m", vec![Message::text(MessageRole::User, "first")]),
            Arc::new(()),
            control,
            &Unsupported,
        )
        .await;
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    assert!(p.requests.lock().unwrap().is_empty());
    assert!(
        handle
            .submit_custom(CustomDelivery::TurnEnd, custom("late"))
            .await
            .is_err()
    );
    let (handle, _control) = RunHandle::pair();
    assert!(
        handle
            .begin_input(RunInputKind::Context, "user".into())
            .is_err()
    );
    assert!(
        handle
            .submit_custom(CustomDelivery::NextTurn, custom("no session"))
            .await
            .is_err()
    );
}

struct ToolModel(AtomicUsize);
#[async_trait]
impl ModelProvider for ToolModel {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", model);
        s.capabilities.tools = true;
        Ok(s)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: if first {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "wait".into(),
                            name: "wait".into(),
                            arguments: json!({}),
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::Assistant, "answer")
            },
            stop_reason: if first {
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
struct WaitTool {
    started: Arc<Notify>,
    release: Arc<Notify>,
    effects: Arc<AtomicUsize>,
}
#[async_trait]
impl aicoder_core::tools::ExecutableTool for WaitTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "wait".into(),
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
        self.started.notify_one();
        self.release.notified().await;
        self.effects.fetch_add(1, Ordering::SeqCst);
        Ok(aicoder_core::tools::ToolSuccess::new(json!(
            "tool complete"
        )))
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(event.clone());
    }
}
#[tokio::test]
async fn custom_delivery_waits_for_paired_tool_boundaries_and_never_repeats_a_running_operation() {
    for delivery in [
        CustomDelivery::Steer,
        CustomDelivery::FollowUp,
        CustomDelivery::TurnEnd,
    ] {
        let root = tempfile::tempdir().unwrap();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let effects = Arc::new(AtomicUsize::new(0));
        let mut tools = aicoder_core::tools::ToolRegistry::default();
        tools
            .register(WaitTool {
                started: started.clone(),
                release: release.clone(),
                effects: effects.clone(),
            })
            .unwrap();
        let p = Arc::new(ToolModel(AtomicUsize::new(0)));
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(p.clone())
                .workspace(root.path())
                .registry(tools)
                .build()
                .unwrap(),
            AgentConfig::new("m"),
        );
        let (handle, control) = agent
            .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
            .unwrap();
        let events = Arc::new(Events::default());
        let trace = events.clone();
        let run =
            tokio::spawn(async move { agent.run_controlled("original", trace, control).await });
        wait(&started).await;
        handle
            .submit_custom(delivery, custom("TOOL_BOUNDARY_CONTEXT"))
            .await
            .unwrap();
        release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap()
            .into_result()
            .unwrap();
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(
            p.0.load(Ordering::SeqCst),
            if delivery == CustomDelivery::FollowUp {
                3
            } else {
                2
            }
        );
        let history = &result.execution_result.messages;
        let custom_index = history
            .iter()
            .position(|m| m.role == MessageRole::Custom)
            .unwrap();
        let tool_index = history
            .iter()
            .position(|m| m.role == MessageRole::Tool)
            .unwrap();
        assert!(tool_index < custom_index);
        let mut validator = EventTraceValidator::new();
        for event in events.0.lock().unwrap().iter() {
            validator.observe(event).unwrap();
        }
        validator.finish().unwrap();
    }
}
#[tokio::test]
async fn late_or_oversized_custom_submissions_preserve_the_original_and_never_bypass_queue_bounds()
{
    let root = tempfile::tempdir().unwrap();
    let agent = agent(root.path(), Arc::new(Provider::default()));
    let (handle, control) = agent
        .control_channel(
            TurnExecutionContext::new(),
            RunControlConfig {
                max_pending_inputs: 1,
                ..Default::default()
            },
        )
        .unwrap();
    let first = handle
        .begin_custom(CustomDelivery::TurnEnd, custom("first"))
        .unwrap();
    let error = match handle.begin_custom(CustomDelivery::TurnEnd, custom("second")) {
        Err(error) => error,
        Ok(_) => panic!(),
    };
    assert_eq!(
        error.error.downcast_ref::<RunControlError>(),
        Some(&RunControlError::QueueFull)
    );
    assert!(error.message.message().text_content().contains("second"));
    drop(first);
    let large = CustomMessage::text("s", "k", "x".repeat(65536)).unwrap();
    assert!(
        handle
            .submit_custom(CustomDelivery::Steer, large)
            .await
            .is_err()
    );
    drop(control);
    assert!(
        handle
            .submit_custom(CustomDelivery::TurnEnd, custom("late"))
            .await
            .is_err()
    );
}
#[tokio::test]
async fn cancelled_run_keeps_accepted_next_turn_data_but_does_not_restore_it_from_serialized_history()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider {
        gate: true,
        ..Default::default()
    });
    let agent = agent(root.path(), p.clone());
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let owner = agent.clone();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let run = tokio::spawn(async move {
        owner
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&p.started).await;
    handle
        .submit_custom(CustomDelivery::NextTurn, custom("AFTER_CANCEL"))
        .await
        .unwrap();
    handle.cancel("cancel").unwrap();
    assert!(matches!(run.await.unwrap(), AgentTurnOutcome::Aborted(_)));
    assert_eq!(agent.custom_inbox().pending(&id).len(), 1);
    assert!(
        !serde_json::to_string(repo.open(&id).unwrap().entries())
            .unwrap()
            .contains("AFTER_CANCEL")
    );
    agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(id.clone()),
            "resume",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert!(agent.custom_inbox().pending(&id).is_empty());
    assert!(
        serde_json::to_string(p.requests.lock().unwrap().last().unwrap())
            .unwrap()
            .contains("AFTER_CANCEL")
    );
}

struct ClaimGate {
    calls: AtomicUsize,
    started: Arc<Notify>,
    release: Arc<Notify>,
}
#[async_trait]
impl BeforeStartHook for ClaimGate {
    async fn before_start(
        &self,
        _: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(None)
    }
}
#[tokio::test]
async fn a_next_turn_claim_keeps_new_arrivals_for_the_following_run_and_rejects_discard_while_claimed()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider {
        gate: true,
        ..Default::default()
    });
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut e = Extension::new("claim", "1").unwrap();
    e.before_start_hook(
        HookSpec::new("gate"),
        Arc::new(ClaimGate {
            calls: AtomicUsize::new(0),
            started: started.clone(),
            release: release.clone(),
        }),
    )
    .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let agent = Arc::new(Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    ));
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let owner = agent.clone();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let first = tokio::spawn(async move {
        owner
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&p.started).await;
    let CustomSubmission::NextTurn { queue_id } = handle
        .submit_custom(CustomDelivery::NextTurn, custom("CLAIM_A"))
        .await
        .unwrap()
    else {
        panic!()
    };
    p.release.notify_one();
    first.await.unwrap().into_result().unwrap();
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let owner = agent.clone();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let second = tokio::spawn(async move {
        owner
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "second",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&started).await;
    assert_eq!(
        agent.custom_inbox().discard(&id, queue_id),
        Err(RunControlError::CustomBusy)
    );
    handle
        .submit_custom(CustomDelivery::NextTurn, custom("CLAIM_B"))
        .await
        .unwrap();
    release.notify_one();
    second.await.unwrap().into_result().unwrap();
    assert_eq!(agent.custom_inbox().pending(&id).len(), 1);
    let body = serde_json::to_string(p.requests.lock().unwrap().last().unwrap()).unwrap();
    assert!(body.contains("CLAIM_A") && !body.contains("CLAIM_B"));
    agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(id.clone()),
            "third",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert!(agent.custom_inbox().pending(&id).is_empty());
    assert!(
        serde_json::to_string(p.requests.lock().unwrap().last().unwrap())
            .unwrap()
            .contains("CLAIM_B")
    );
}
struct CustomJournal {
    fail: bool,
    slow: bool,
    commits: AtomicUsize,
}
#[async_trait]
impl RunJournal for CustomJournal {
    fn supports_custom_input(&self) -> bool {
        true
    }
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn input_custom(&self, _: usize, input: &RunInput) -> Result<()> {
        input.custom_message()?.unwrap();
        self.commits.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            anyhow::bail!("custom commit refused")
        }
        if self.slow {
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn failed_or_slow_custom_commits_keep_paid_usage_and_never_repeat_the_completed_model() {
    for slow in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let executor = TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_duration: Some(Duration::from_millis(200)),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap();
        let (handle, control) = executor
            .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
            .unwrap();
        handle
            .submit_custom(CustomDelivery::FollowUp, custom("AFTER_ACK"))
            .await
            .unwrap();
        let journal = CustomJournal {
            fail: !slow,
            slow,
            commits: AtomicUsize::new(0),
        };
        let outcome = executor
            .run_controlled_with_journal(
                ModelRequest::new("m", vec![Message::text(MessageRole::User, "first")]),
                Arc::new(()),
                control,
                &journal,
            )
            .await;
        assert_eq!(journal.commits.load(Ordering::SeqCst), 1);
        assert_eq!(p.requests.lock().unwrap().len(), 1);
        assert_eq!(outcome.progress().usage.total_tokens, Some(7));
        assert!(
            outcome
                .progress()
                .messages
                .iter()
                .any(|m| m.role == MessageRole::Custom)
        );
        assert!(
            matches!(outcome,RunOutcome::Interrupted(ref i)if if slow{matches!(i.stop,RunStop::Budget(_))}else{matches!(i.stop,RunStop::Failed(_))})
        );
        assert_eq!(outcome.progress().pending_inputs.is_empty(), slow);
    }
}
struct HandleStatus;
#[async_trait]
impl InputHook for HandleStatus {
    async fn input(
        &self,
        input: InputHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<InputAction> {
        Ok(if input.input.message().text_content() == "status" {
            InputAction::Handled
        } else {
            InputAction::Continue
        })
    }
}
#[tokio::test]
async fn a_handled_initial_input_does_not_claim_or_consume_next_turn_context() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider {
        gate: true,
        ..Default::default()
    });
    let mut e = Extension::new("status", "1").unwrap();
    e.input_hook(HookSpec::new("handle"), Arc::new(HandleStatus))
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let agent = Arc::new(Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    ));
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let owner = agent.clone();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let run = tokio::spawn(async move {
        owner
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&p.started).await;
    handle
        .submit_custom(CustomDelivery::NextTurn, custom("FOR_REAL_TRIGGER_ONLY"))
        .await
        .unwrap();
    p.release.notify_one();
    run.await.unwrap().into_result().unwrap();
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(id.clone()),
            "status",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome,AgentTurnOutcome::Stopped(ref t)if matches!(t.stop,RunStop::InputHandled(_)))
    );
    assert_eq!(agent.custom_inbox().pending(&id).len(), 1);
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(id.clone()),
            "next real trigger",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert!(agent.custom_inbox().pending(&id).is_empty());
}

#[tokio::test]
async fn deferred_custom_context_cannot_poison_a_known_window_before_becoming_a_recorded_fact() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider {
        gate: true,
        window: Some(128),
        ..Default::default()
    });
    let agent = agent(root.path(), p.clone());
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let run = tokio::spawn(async move {
        agent
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&p.started).await;
    handle
        .submit_custom(CustomDelivery::TurnEnd, custom(&"x".repeat(2048)))
        .await
        .unwrap();
    p.release.notify_one();
    let outcome = run.await.unwrap();
    assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert_eq!(outcome.progress().usage.total_tokens, Some(7));
    assert!(
        repo.open(&id)
            .unwrap()
            .messages()
            .iter()
            .all(|m| m.message.role != MessageRole::Custom)
    );
    assert_eq!(outcome.progress().pending_inputs.len(), 1);
}

#[tokio::test]
async fn next_turn_inbox_transfer_is_explicit_workspace_bound_and_records_consumption_origin_once()
{
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider {
        gate: true,
        ..Default::default()
    });
    let agent = agent(root.path(), p.clone());
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let origin = handle.run_id();
    let owner = agent.clone();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let run = tokio::spawn(async move {
        owner
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&p.started).await;
    let CustomSubmission::NextTurn { queue_id } = handle
        .submit_custom(CustomDelivery::NextTurn, custom("TRANSFER_CONTEXT"))
        .await
        .unwrap()
    else {
        panic!()
    };
    p.release.notify_one();
    run.await.unwrap().into_result().unwrap();
    let other = tempfile::tempdir().unwrap();
    let wrong = Agent::new(
        TurnExecutor::builder(Provider::default())
            .workspace(other.path())
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    assert!(wrong.with_custom_inbox(agent.custom_inbox()).is_err());
    let replacement = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    )
    .with_custom_inbox(agent.custom_inbox())
    .unwrap();
    replacement
        .run_with_session(
            &repo,
            SessionSelection::Existing(id.clone()),
            "next generation",
            Arc::new(()),
        )
        .await
        .unwrap();
    let saved = repo.open(&id).unwrap();
    let receipts = saved
        .entries()
        .iter()
        .filter_map(|e| match &e.data {
            SessionEntryData::Extension { namespace, data }
                if namespace == "aicoder.custom-next-turn.v1" =>
            {
                Some(data)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["queue_id"], queue_id);
    assert_eq!(receipts[0]["queued_run_id"], origin.to_string());
    assert!(replacement.custom_inbox().pending(&id).is_empty());
}

struct RetryGate {
    started: Arc<Notify>,
    release: Arc<Notify>,
}
#[async_trait]
impl CompletionVerifier for RetryGate {
    async fn verify(&self, context: CompletionContext<'_>) -> Result<CompletionVerdict> {
        if context.round == 1 {
            self.started.notify_one();
            self.release.notified().await;
            Ok(CompletionVerdict::Rejected {
                feedback: "retry after verification".into(),
            })
        } else {
            Ok(CompletionVerdict::Accepted)
        }
    }
}
#[tokio::test]
async fn context_received_during_rejected_verification_is_visible_to_the_immediate_retry_request() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider::default());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .completion_verifier(RetryGate {
                started: started.clone(),
                release: release.clone(),
            })
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let run = tokio::spawn(async move {
        agent
            .run_with_session_controlled(
                &repo,
                SessionSelection::Existing(id),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    wait(&started).await;
    handle
        .submit_custom(CustomDelivery::TurnEnd, custom("BEFORE_IMMEDIATE_RETRY"))
        .await
        .unwrap();
    release.notify_one();
    run.await.unwrap().into_result().unwrap();
    let requests = p.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        serde_json::to_string(&requests[1].messages)
            .unwrap()
            .contains("BEFORE_IMMEDIATE_RETRY")
    );
}
