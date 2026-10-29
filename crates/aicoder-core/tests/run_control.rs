use aicoder_core::{model::*, session::*, tools::*, *};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Default)]
struct Gate {
    started: Notify,
    release: Notify,
}
impl Gate {
    async fn enter(&self) {
        self.started.notify_one();
        self.release.notified().await;
    }
    async fn wait(&self) {
        tokio::time::timeout(Duration::from_secs(2), self.started.notified())
            .await
            .unwrap();
    }
}
struct Step {
    response: ModelResponse,
    gate: Option<Arc<Gate>>,
}
struct Script {
    steps: Mutex<VecDeque<Step>>,
    requests: Mutex<Vec<ModelRequest>>,
}
impl Script {
    fn new(responses: Vec<ModelResponse>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(
                responses
                    .into_iter()
                    .map(|response| Step {
                        response,
                        gate: None,
                    })
                    .collect(),
            ),
            requests: Mutex::new(vec![]),
        })
    }
}
#[async_trait]
impl ModelProvider for Script {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("script", model);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.requests.lock().unwrap().push(request);
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted");
        if let Some(gate) = step.gate {
            gate.enter().await;
        }
        emit_response(&step.response, sink.as_ref());
        Ok(step.response)
    }
}
fn user(text: &str) -> Message {
    Message::text(MessageRole::User, text)
}
fn reply(text: &str) -> ModelResponse {
    ModelResponse {
        id: None,
        model: "test".into(),
        message: Message::text(MessageRole::Assistant, text),
        stop_reason: StopReason::Complete,
        usage: ModelUsage {
            total_tokens: Some(1),
            ..Default::default()
        },
    }
}
fn calls(names: &[&str]) -> ModelResponse {
    let mut response = reply("");
    response.stop_reason = StopReason::ToolCalls;
    response.message.content = names
        .iter()
        .enumerate()
        .map(|(i, name)| ContentBlock::ToolCall {
            call: ModelToolCall {
                id: format!("c{i}"),
                name: (*name).into(),
                arguments: json!({}),
            },
        })
        .collect();
    response
}
fn request() -> ModelRequest {
    ModelRequest::new("test", vec![user("initial")])
}
fn executor(
    script: Arc<Script>,
    registry: ToolRegistry,
    workspace: &std::path::Path,
) -> TurnExecutor {
    TurnExecutor::builder_from_shared(script)
        .workspace(workspace)
        .registry(registry)
        .approval(AllowAllApproval)
        .build()
        .unwrap()
}
struct CommandTool {
    name: &'static str,
    gate: Option<Arc<Gate>>,
    executed: Arc<AtomicUsize>,
}
#[async_trait]
impl ExecutableTool for CommandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        if let Some(gate) = &self.gate {
            gate.enter().await;
        }
        self.executed.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess {
            output: json!("changed"),
            truncated: false,
        })
    }
}
async fn approval(handle: &RunHandle) -> PendingRunApproval {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = handle.snapshot();
            if let Some(approval) = snapshot.approvals.into_iter().next() {
                return approval;
            }
            assert!(
                matches!(
                    snapshot.status,
                    RunControlStatus::Waiting | RunControlStatus::Running
                ),
                "run ended before requesting approval"
            );
            handle.changed(snapshot.revision).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn queued_steering_has_priority_and_follow_ups_extend_the_same_budgeted_run() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![reply("one"), reply("two"), reply("three")]);
    let executor = executor(script.clone(), ToolRegistry::default(), workspace.path());
    let (handle, control) = RunHandle::pair();
    handle.follow_up(user("follow one")).unwrap();
    handle.steer(user("steer one")).unwrap();
    handle.follow_up(user("follow two")).unwrap();
    handle.steer(user("steer two")).unwrap();
    let events = Arc::new(Mutex::new(vec![]));
    let target = events.clone();
    let handler =
        Arc::new(move |event: &AgentRawEventEnvelope| target.lock().unwrap().push(event.clone()));
    let outcome = executor.run_controlled(request(), handler, control).await;
    let RunOutcome::Completed(result) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(result.rounds, 3);
    assert_eq!(result.usage.total_tokens, Some(3));
    assert!(result.pending_inputs.is_empty());
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.status, RunControlStatus::Finished);
    assert!(snapshot.pending_inputs.is_empty());
    assert_eq!(
        handle.follow_up(user("too late")).unwrap_err().error,
        RunControlError::Closed
    );
    let requests = script.requests.lock().unwrap();
    assert_eq!(
        requests[0].messages,
        vec![user("initial"), user("steer one"), user("steer two")]
    );
    assert_eq!(requests[1].messages.last(), Some(&user("follow one")));
    assert_eq!(requests[2].messages.last(), Some(&user("follow two")));
    let events = events.lock().unwrap();
    assert!(events.iter().all(|event| event.run_id == handle.run_id()));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, AgentRawEvent::RunInputApplied { .. }))
            .count(),
        4
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, AgentRawEvent::AgentCompleted { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn steering_during_a_model_request_pairs_skipped_calls_without_executing_them() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![calls(&["write"]), reply("new goal")]);
    let gate = Arc::new(Gate::default());
    script.steps.lock().unwrap()[0].gate = Some(gate.clone());
    let executed = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry
        .register(CommandTool {
            name: "write",
            gate: None,
            executed: executed.clone(),
        })
        .unwrap();
    let executor = executor(script.clone(), registry, workspace.path());
    let (handle, control) = RunHandle::pair();
    let run = tokio::spawn(async move {
        executor
            .run_controlled(request(), Arc::new(()), control)
            .await
    });
    gate.wait().await;
    handle.steer(user("redirect")).unwrap();
    gate.release.notify_one();
    assert!(matches!(run.await.unwrap(), RunOutcome::Completed(_)));
    assert_eq!(executed.load(Ordering::SeqCst), 0);
    let requests = script.requests.lock().unwrap();
    assert_eq!(
        requests[1].messages[2].tool_result().unwrap().1["error"]["code"],
        "steered_before_execution"
    );
}

#[tokio::test]
async fn approval_ids_are_scoped_and_cannot_be_reused_when_a_model_reuses_call_ids() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![calls(&["write"]), calls(&["write"]), reply("done")]);
    let executed = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry
        .register(CommandTool {
            name: "write",
            gate: None,
            executed: executed.clone(),
        })
        .unwrap();
    let executor = executor(script, registry, workspace.path());
    let (handle, control) = RunHandle::channel(
        TurnExecutionContext::new(),
        RunControlConfig {
            handle_approvals: true,
            ..Default::default()
        },
    )
    .unwrap();
    let run = tokio::spawn(async move {
        executor
            .run_controlled(request(), Arc::new(()), control)
            .await
    });
    let first = approval(&handle).await;
    let mut foreign = first.id.clone();
    foreign.run_id = "other run".into();
    assert_eq!(
        handle.respond_approval(&foreign, true),
        Err(RunControlError::UnknownApproval)
    );
    handle.respond_approval(&first.id, true).unwrap();
    let second = approval(&handle).await;
    assert_ne!(first.id, second.id);
    assert_eq!(first.invocation.call_id, second.invocation.call_id);
    assert_eq!(
        handle.respond_approval(&first.id, true),
        Err(RunControlError::UnknownApproval)
    );
    handle.respond_approval(&second.id, false).unwrap();
    assert!(matches!(run.await.unwrap(), RunOutcome::Completed(_)));
    assert_eq!(executed.load(Ordering::SeqCst), 1);
    assert!(handle.snapshot().approvals.is_empty());
    assert_eq!(
        handle.respond_approval(&second.id, true),
        Err(RunControlError::Closed)
    );
}

#[tokio::test]
async fn steering_supersedes_a_pending_approval_without_authorizing_the_tool() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![calls(&["write"]), reply("redirected")]);
    let executed = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry
        .register(CommandTool {
            name: "write",
            gate: None,
            executed: executed.clone(),
        })
        .unwrap();
    let executor = executor(script.clone(), registry, workspace.path());
    let (handle, control) = RunHandle::channel(
        TurnExecutionContext::new(),
        RunControlConfig {
            handle_approvals: true,
            ..Default::default()
        },
    )
    .unwrap();
    let run = tokio::spawn(async move {
        executor
            .run_controlled(request(), Arc::new(()), control)
            .await
    });
    let pending = approval(&handle).await;
    handle.steer(user("do something else")).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(_)), "{outcome:?}");
    assert_eq!(executed.load(Ordering::SeqCst), 0);
    assert!(handle.snapshot().approvals.is_empty());
    assert_eq!(
        handle.respond_approval(&pending.id, true),
        Err(RunControlError::Closed)
    );
    assert_eq!(
        script.requests.lock().unwrap()[1].messages[2]
            .tool_result()
            .unwrap()
            .1["error"]["code"],
        "steered_before_execution"
    );
}

#[tokio::test]
async fn a_follow_up_at_the_round_limit_remains_pending_and_is_persisted_with_the_stop() {
    let workspace = tempfile::tempdir().unwrap();
    let store = MemorySessionRepository::new();
    let script = Script::new(vec![reply("initial answer")]);
    let executor = TurnExecutor::builder_from_shared(script.clone())
        .workspace(workspace.path())
        .registry(ToolRegistry::default())
        .config(TurnExecutionConfig {
            max_rounds: 1,
            ..Default::default()
        })
        .build()
        .unwrap();
    let agent = Agent::new(executor, AgentConfig::new("test"));
    let (handle, control) = RunHandle::pair();
    let input = handle.follow_up(user("later work")).unwrap();
    let outcome = agent
        .run_with_session_controlled(
            &store,
            SessionSelection::New,
            "initial",
            Arc::new(()),
            control,
        )
        .await;
    assert!(
        matches!(&outcome,AgentTurnOutcome::Limited(turn) if matches!(&turn.stop,RunStop::Budget(limit) if limit.resource==BudgetResource::Rounds))
    );
    assert_eq!(outcome.progress().pending_inputs[0].id, input);
    let session = store.open(&outcome.session().unwrap().id).unwrap();
    assert_eq!(session.messages().len(), 2);
    assert!(
        matches!(&session.active_entries().last().unwrap().data,SessionEntryData::TurnEnd {record,..} if record.pending_inputs[0].id==input)
    );
    assert_eq!(handle.snapshot().pending_inputs.len(), 1);
    assert_eq!(script.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancel_closes_pending_approval_and_retains_queued_follow_up() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![calls(&["write"])]);
    let executed = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry
        .register(CommandTool {
            name: "write",
            gate: None,
            executed: executed.clone(),
        })
        .unwrap();
    let executor = executor(script, registry, workspace.path());
    let (handle, control) = RunHandle::channel(
        TurnExecutionContext::new(),
        RunControlConfig {
            handle_approvals: true,
            ..Default::default()
        },
    )
    .unwrap();
    handle.follow_up(user("pending work")).unwrap();
    let run = tokio::spawn(async move {
        executor
            .run_controlled(request(), Arc::new(()), control)
            .await
    });
    let pending = approval(&handle).await;
    handle.cancel("host stop").unwrap();
    let RunOutcome::Interrupted(turn) = run.await.unwrap() else {
        panic!("run completed")
    };
    assert!(matches!(turn.stop, RunStop::Cancelled { .. }));
    assert_eq!(turn.pending_inputs.len(), 1);
    assert_eq!(executed.load(Ordering::SeqCst), 0);
    assert!(handle.snapshot().approvals.is_empty());
    assert_eq!(
        handle.respond_approval(&pending.id, true),
        Err(RunControlError::Closed)
    );
}

#[test]
fn queues_are_bounded_and_rejected_messages_are_returned_to_the_caller() {
    let (handle, control) = RunHandle::channel(
        TurnExecutionContext::new(),
        RunControlConfig {
            max_pending_inputs: 1,
            max_input_bytes: 128,
            max_pending_bytes: 128,
            ..Default::default()
        },
    )
    .unwrap();
    handle.steer(user("first")).unwrap();
    let rejected = handle.follow_up(user("second")).unwrap_err();
    assert_eq!(rejected.error, RunControlError::QueueFull);
    assert_eq!(rejected.message, user("second"));
    assert_eq!(
        handle.steer(user(&"x".repeat(1024))).unwrap_err().error,
        RunControlError::InputTooLarge
    );
    assert_eq!(
        handle
            .steer(Message::text(MessageRole::System, "invalid"))
            .unwrap_err()
            .error,
        RunControlError::InvalidInput
    );
    drop(control);
    assert_eq!(handle.snapshot().status, RunControlStatus::Dropped);
    assert_eq!(handle.snapshot().pending_inputs.len(), 1);
    assert_eq!(
        handle.steer(user("closed")).unwrap_err().error,
        RunControlError::Closed
    );
}

#[tokio::test]
async fn dropping_only_the_host_handle_does_not_cancel_the_run() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![reply("done")]);
    let gate = Arc::new(Gate::default());
    script.steps.lock().unwrap()[0].gate = Some(gate.clone());
    let executor = executor(script, ToolRegistry::default(), workspace.path());
    let (handle, control) = RunHandle::pair();
    let run = tokio::spawn(async move {
        executor
            .run_controlled(request(), Arc::new(()), control)
            .await
    });
    gate.wait().await;
    drop(handle);
    gate.release.notify_one();
    assert!(matches!(run.await.unwrap(), RunOutcome::Completed(_)));
}

#[tokio::test]
async fn dropping_the_running_future_closes_controls_and_keeps_unconsumed_inputs() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![reply("never delivered")]);
    let gate = Arc::new(Gate::default());
    script.steps.lock().unwrap()[0].gate = Some(gate.clone());
    let executor = executor(script, ToolRegistry::default(), workspace.path());
    let (handle, control) = RunHandle::pair();
    handle.follow_up(user("must remain available")).unwrap();
    let run = tokio::spawn(async move {
        executor
            .run_controlled(request(), Arc::new(()), control)
            .await
    });
    gate.wait().await;
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.status, RunControlStatus::Dropped);
    assert_eq!(
        snapshot.pending_inputs[0].message,
        user("must remain available")
    );
    assert!(snapshot.approvals.is_empty());
    assert_eq!(handle.cancel("late"), Err(RunControlError::Closed));
}

#[tokio::test]
async fn early_session_failure_and_pre_cancellation_close_controls_without_losing_inputs() {
    let workspace = tempfile::tempdir().unwrap();
    let store = MemorySessionRepository::new();
    let script = Script::new(vec![]);
    let agent = Agent::new(
        executor(script.clone(), ToolRegistry::default(), workspace.path()),
        AgentConfig::new("test"),
    );
    let (handle, control) = RunHandle::pair();
    handle.follow_up(user("queued")).unwrap();
    let outcome = agent
        .run_with_session_controlled(
            &store,
            SessionSelection::Existing("missing".into()),
            "initial",
            Arc::new(()),
            control,
        )
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
    assert_eq!(outcome.progress().pending_inputs.len(), 1);
    assert_eq!(handle.snapshot().status, RunControlStatus::Finished);
    let (handle, control) = RunHandle::pair();
    handle.follow_up(user("queued")).unwrap();
    handle.cancel("before starting").unwrap();
    let outcome = agent
        .run_with_session_controlled(
            &store,
            SessionSelection::New,
            "initial",
            Arc::new(()),
            control,
        )
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Aborted(_)));
    assert_eq!(outcome.progress().pending_inputs.len(), 1);
    assert!(store.list(workspace.path()).unwrap().is_empty());
    assert!(script.requests.lock().unwrap().is_empty());
    assert_eq!(handle.snapshot().status, RunControlStatus::Finished);
}

#[tokio::test]
async fn terminal_callbacks_cannot_enqueue_unreported_work() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![reply("done")]);
    let executor = executor(script, ToolRegistry::default(), workspace.path());
    let (handle, control) = RunHandle::pair();
    let target = handle.clone();
    let rejected = Arc::new(AtomicUsize::new(0));
    let counted = rejected.clone();
    let handler = Arc::new(move |event: &AgentRawEventEnvelope| {
        if matches!(event.event, AgentRawEvent::AgentCompleted { .. }) {
            assert_eq!(target.snapshot().status, RunControlStatus::Finished);
            assert_eq!(
                target.follow_up(user("too late")).unwrap_err().error,
                RunControlError::Closed
            );
            counted.fetch_add(1, Ordering::SeqCst);
        }
    });
    let outcome = executor.run_controlled(request(), handler, control).await;
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert!(outcome.progress().pending_inputs.is_empty());
    assert_eq!(rejected.load(Ordering::SeqCst), 1);
}

struct FailInputJournal;
#[async_trait]
impl RunJournal for FailInputJournal {
    async fn checkpoint(&self, _: usize, _: &Message) -> anyhow::Result<()> {
        Ok(())
    }
    async fn input(&self, _: usize, _: &RunInput) -> anyhow::Result<()> {
        anyhow::bail!("input storage unavailable")
    }
    async fn finish(&self, _: &RunOutcome) -> anyhow::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn failed_input_checkpoint_preserves_the_input_and_never_starts_a_model_request() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![]);
    let executor = executor(script.clone(), ToolRegistry::default(), workspace.path());
    let (handle, control) = RunHandle::pair();
    let id = handle.steer(user("must save first")).unwrap();
    let outcome = executor
        .run_controlled_with_journal(request(), Arc::new(()), control, &FailInputJournal)
        .await;
    assert!(
        matches!(&outcome,RunOutcome::Interrupted(turn) if matches!(&turn.stop,RunStop::Failed(failure) if failure.stage==RunStage::Persistence))
    );
    assert_eq!(
        outcome.progress().messages.last(),
        Some(&user("must save first"))
    );
    assert_eq!(outcome.progress().pending_inputs[0].id, id);
    assert_eq!(handle.snapshot().pending_inputs[0].id, id);
    assert!(script.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn external_cancellation_drops_the_approval_sender_and_rejects_late_decisions() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![calls(&["write"])]);
    let executed = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry
        .register(CommandTool {
            name: "write",
            gate: None,
            executed: executed.clone(),
        })
        .unwrap();
    let executor = executor(script, registry, workspace.path());
    let parent = TurnExecutionContext::new();
    let (handle, control) = RunHandle::channel(
        parent.clone(),
        RunControlConfig {
            handle_approvals: true,
            ..Default::default()
        },
    )
    .unwrap();
    let run = tokio::spawn(async move {
        executor
            .run_controlled(request(), Arc::new(()), control)
            .await
    });
    let pending = approval(&handle).await;
    parent.cancel("parent stopped");
    assert!(
        matches!(run.await.unwrap(),RunOutcome::Interrupted(turn) if matches!(turn.stop,RunStop::Cancelled {..}))
    );
    assert!(handle.snapshot().approvals.is_empty());
    assert_eq!(
        handle.respond_approval(&pending.id, true),
        Err(RunControlError::Closed)
    );
    assert_eq!(executed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pending_byte_capacity_is_released_after_application_independently_of_count_limits() {
    let workspace = tempfile::tempdir().unwrap();
    let script = Script::new(vec![reply("first"), reply("second")]);
    let gate = Arc::new(Gate::default());
    script.steps.lock().unwrap()[0].gate = Some(gate.clone());
    let executor = executor(script, ToolRegistry::default(), workspace.path());
    let (handle, control) = RunHandle::channel(
        TurnExecutionContext::new(),
        RunControlConfig {
            max_pending_inputs: 4,
            max_input_bytes: 256,
            max_pending_bytes: 300,
            handle_approvals: false,
        },
    )
    .unwrap();
    let message = user(&"x".repeat(150));
    handle.steer(message.clone()).unwrap();
    assert_eq!(
        handle.follow_up(message.clone()).unwrap_err().error,
        RunControlError::QueueFull
    );
    let run = tokio::spawn(async move {
        executor
            .run_controlled(request(), Arc::new(()), control)
            .await
    });
    gate.wait().await;
    assert!(handle.snapshot().pending_inputs.is_empty());
    handle.follow_up(message).unwrap();
    gate.release.notify_one();
    let result = run.await.unwrap().into_result().unwrap();
    assert_eq!(result.rounds, 2);
    assert!(handle.snapshot().pending_inputs.is_empty());
}
