use aicoder_core::{events::*, extensions::*, model::*, session::*, *};
use anyhow::Result;
use async_trait::async_trait;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
#[derive(Default)]
struct Provider {
    requests: Mutex<Vec<ModelRequest>>,
    wait_first: bool,
    window: Option<u64>,
    started: Notify,
    release: Notify,
    dropped: Arc<AtomicUsize>,
}
struct Attempt {
    done: bool,
    dropped: Arc<AtomicUsize>,
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.done {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}
fn usage(tokens: u64) -> ModelUsage {
    ModelUsage {
        input_tokens: Some(tokens),
        output_tokens: Some(0),
        total_tokens: Some(tokens),
        cached_input_tokens: Some(0),
        cache_write_tokens: Some(0),
    }
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", model);
        s.capabilities.images = true;
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
        let mut guard = Attempt {
            done: false,
            dropped: self.dropped.clone(),
        };
        sink.emit(ModelEvent::Started);
        if first && self.wait_first {
            sink.emit(ModelEvent::Usage { usage: usage(3) });
            self.started.notify_one();
            self.release.notified().await;
        }
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "answer"),
            stop_reason: StopReason::Complete,
            usage: usage(7),
        };
        sink.emit(ModelEvent::TextDelta {
            delta: "answer".into(),
        });
        sink.emit(ModelEvent::Usage {
            usage: response.usage.clone(),
        });
        sink.emit(ModelEvent::Finished {
            stop_reason: StopReason::Complete,
        });
        guard.done = true;
        Ok(response)
    }
}
async fn notified(n: &Notify) {
    tokio::time::timeout(Duration::from_secs(2), n.notified())
        .await
        .unwrap();
}
fn prompt(input: &AgentInput) -> String {
    match input.message().content.first() {
        Some(ContentBlock::Text { text }) => text.clone(),
        _ => String::new(),
    }
}
#[derive(Clone, Copy)]
enum Mode {
    TransformAll,
    TransformControls,
    HandleInitial,
    HandleControls,
    Fail,
    Panic,
    Huge,
}
struct Hook {
    mode: Mode,
    calls: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<(InputSource, InputDelivery, String)>>>,
}
#[async_trait]
impl InputHook for Hook {
    async fn input(
        &self,
        input: InputHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<InputAction> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .unwrap()
            .push((input.source, input.delivery, prompt(input.input)));
        Ok(match self.mode {
            Mode::HandleInitial if input.delivery == InputDelivery::Initial => InputAction::Handled,
            Mode::HandleControls if input.delivery != InputDelivery::Initial => {
                InputAction::Handled
            }
            Mode::TransformAll => InputAction::Transform(
                input
                    .input
                    .clone()
                    .with_prompt_text("EFFECTIVE")
                    .with_source(InputSource::Web),
            ),
            Mode::TransformControls if input.delivery != InputDelivery::Initial => {
                InputAction::Transform(input.input.clone().with_prompt_text("EFFECTIVE_CONTROL"))
            }
            Mode::Fail => anyhow::bail!("PRIVATE_NATIVE_ERROR"),
            Mode::Panic => panic!("PRIVATE_NATIVE_PANIC"),
            Mode::Huge => InputAction::Transform(
                input
                    .input
                    .clone()
                    .with_prompt_text("x".repeat(2 * 1024 * 1024)),
            ),
            _ => InputAction::Continue,
        })
    }
}
type Seen = Arc<Mutex<Vec<(InputSource, InputDelivery, String)>>>;
fn hook(mode: Mode) -> (Arc<dyn InputHook>, Arc<AtomicUsize>, Seen) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(vec![]));
    (
        Arc::new(Hook {
            mode,
            calls: calls.clone(),
            seen: seen.clone(),
        }),
        calls,
        seen,
    )
}
fn extensions(h: Arc<dyn InputHook>) -> Arc<ExtensionSet> {
    let mut e = Extension::new("input", "1").unwrap();
    e.input_hook(HookSpec::new("raw"), h).unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    Arc::new(r.finish())
}
fn executor(root: &std::path::Path, p: Arc<Provider>, h: Arc<dyn InputHook>) -> TurnExecutor {
    TurnExecutor::builder_from_shared(p)
        .workspace(root)
        .extensions(extensions(h))
        .build()
        .unwrap()
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, e: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(e.clone());
    }
}
struct Expand(Arc<AtomicUsize>);
#[async_trait]
impl InputExpander for Expand {
    async fn expand(&self, input: AgentInput) -> Result<AgentInput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(prompt(&input), "EFFECTIVE");
        Ok(input.with_prompt_text("EFFECTIVE_EXPANDED"))
    }
}
#[tokio::test]
async fn initial_transform_precedes_expansion_preserves_attachments_and_omits_raw_input_from_history()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let p = Arc::new(Provider::default());
    let (h, calls, seen) = hook(Mode::TransformAll);
    let expanded = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(h))
            .input_expander(Arc::new(Expand(expanded.clone())))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let message = Message {
        role: MessageRole::User,
        content: vec![
            ContentBlock::Text {
                text: "RAW_PRIVATE_INPUT".into(),
            },
            ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                },
            },
            ContentBlock::Text {
                text: "\nfile snapshot data".into(),
            },
        ],
    };
    let input = AgentInput::try_from(message.clone())
        .unwrap()
        .with_source(InputSource::Rpc);
    let events = Arc::new(Events::default());
    let result = agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            input,
            events.clone(),
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(expanded.load(Ordering::SeqCst), 1);
    assert_eq!(
        seen.lock().unwrap()[0],
        (
            InputSource::Rpc,
            InputDelivery::Initial,
            "RAW_PRIVATE_INPUT".into()
        )
    );
    assert_eq!(result.execution_result.usage.total_tokens, Some(7));
    let requests = p.requests.lock().unwrap();
    let user = requests[0]
        .messages
        .iter()
        .find(|m| m.role == MessageRole::User)
        .unwrap();
    assert_eq!(
        user.content[0],
        ContentBlock::Text {
            text: "EFFECTIVE_EXPANDED".into()
        }
    );
    assert_eq!(user.content[1..], message.content[1..]);
    assert!(
        !serde_json::to_string(&*requests)
            .unwrap()
            .contains("RAW_PRIVATE_INPUT")
    );
    let saved = repo.open(&session.metadata().id).unwrap();
    assert_eq!(
        saved
            .messages()
            .iter()
            .filter(|m| m.message.role == MessageRole::User)
            .count(),
        1
    );
    let records = saved
        .entries()
        .iter()
        .filter_map(|e| match &e.data {
            SessionEntryData::Extension { namespace, data }
                if namespace == INPUT_HOOK_NAMESPACE =>
            {
                Some(serde_json::from_value::<InputHookRecord>(data.clone()).unwrap())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let receipt = records
        .iter()
        .find_map(|r| match r {
            InputHookRecord::Prepared { receipt } => Some(receipt),
            _ => None,
        })
        .unwrap();
    assert_eq!(receipt.source, InputSource::Rpc);
    assert!(receipt.original.is_none());
    assert!(receipt.original_bytes > 0 && receipt.effective_bytes > 0);
    assert!(receipt.expanded && receipt.changes[0].transformed);
    let events = events.0.lock().unwrap();
    assert!(
        !serde_json::to_string(&*events)
            .unwrap()
            .contains("RAW_PRIVATE_INPUT")
    );
    let mut validator = EventTraceValidator::new();
    for e in events.iter() {
        validator.observe(e).unwrap();
    }
    validator.finish().unwrap();
}
#[tokio::test]
async fn handled_initial_input_has_an_explicit_stop_without_model_answer_turn_or_user_history() {
    for session_enabled in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let p = Arc::new(Provider::default());
        let (h, calls, _) = hook(Mode::HandleInitial);
        let agent = Agent::new(executor(root.path(), p.clone(), h), AgentConfig::new("m"));
        let events = Arc::new(Events::default());
        let outcome = if session_enabled {
            agent
                .run_with_session_outcome(
                    &repo,
                    SessionSelection::Existing(session.metadata().id.clone()),
                    "/status",
                    events.clone(),
                    TurnExecutionContext::new(),
                )
                .await
        } else {
            agent
                .run_outcome("/status", events.clone(), TurnExecutionContext::new())
                .await
        };
        assert!(
            matches!(outcome,AgentTurnOutcome::Stopped(ref v)if matches!(v.stop,RunStop::InputHandled(_)))
        );
        assert_eq!(outcome.progress().rounds, 0);
        assert!(outcome.progress().messages.is_empty());
        assert!(
            repo.open(&session.metadata().id)
                .unwrap()
                .entries()
                .is_empty()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(p.requests.lock().unwrap().is_empty());
        let raw = events.0.lock().unwrap();
        let mut validator = EventTraceValidator::new();
        for e in raw.iter() {
            validator.observe(e).unwrap();
        }
        validator.finish().unwrap();
        assert!(
            raw.iter()
                .any(|e| matches!(e.event, AgentRawEvent::AgentStopped { .. }))
        );
    }
}
#[tokio::test]
async fn handled_steering_never_interrupts_the_active_provider_future_or_queues_a_user_message() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        wait_first: true,
        ..Default::default()
    });
    let (h, calls, _) = hook(Mode::HandleControls);
    let agent = Agent::new(executor(root.path(), p.clone(), h), AgentConfig::new("m"));
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let run = tokio::spawn(async move {
        agent
            .run_controlled("original", Arc::new(()), control)
            .await
    });
    notified(&p.started).await;
    assert!(matches!(
        handle.steer(Message::text(MessageRole::User, "must use async")),
        Err(RejectedRunInput {
            error: RunControlError::InputPreparationRequired,
            ..
        })
    ));
    let result = handle
        .submit_input(
            RunInputKind::Steer,
            AgentInput::from("/status").with_source(InputSource::Interactive),
        )
        .await
        .unwrap();
    assert!(matches!(result, InputSubmission::Handled { .. }));
    assert!(handle.snapshot().pending_inputs.is_empty());
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert_eq!(p.dropped.load(Ordering::SeqCst), 0);
    p.release.notify_one();
    let result = run.await.unwrap().into_result().unwrap();
    assert_eq!(result.execution_result.rounds, 1);
    assert_eq!(result.execution_result.usage.total_tokens, Some(7));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn transformed_controls_are_journaled_with_original_provenance_and_share_the_same_metered_loop()
 {
    for kind in [RunInputKind::Steer, RunInputKind::FollowUp] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let saved = repo.create(root.path()).unwrap();
        let id = saved.metadata().id.clone();
        let p = Arc::new(Provider {
            wait_first: true,
            ..Default::default()
        });
        let (h, calls, seen) = hook(Mode::TransformControls);
        let agent = Agent::new(executor(root.path(), p.clone(), h), AgentConfig::new("m"));
        let (handle, control) = agent
            .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
            .unwrap();
        let task_repo = repo.clone();
        let task_id = id.clone();
        let events = Arc::new(Events::default());
        let trace = events.clone();
        let run = tokio::spawn(async move {
            agent
                .run_with_session_controlled(
                    &task_repo,
                    SessionSelection::Existing(task_id),
                    "initial",
                    trace,
                    control,
                )
                .await
        });
        notified(&p.started).await;
        let result = handle
            .submit_input(
                kind,
                AgentInput::from("RAW_CONTROL_PRIVATE").with_source(InputSource::Rpc),
            )
            .await
            .unwrap();
        assert!(matches!(result, InputSubmission::Queued { .. }));
        if kind == RunInputKind::FollowUp {
            assert_eq!(p.dropped.load(Ordering::SeqCst), 0);
            let snapshot = handle.snapshot();
            assert_eq!(
                snapshot.pending_inputs[0].original_message().text_content(),
                "RAW_CONTROL_PRIVATE"
            );
            assert!(
                !serde_json::to_string(&snapshot.pending_inputs)
                    .unwrap()
                    .contains("RAW_CONTROL_PRIVATE")
            );
            p.release.notify_one();
        }
        if kind == RunInputKind::Steer {
            p.release.notify_one();
        }
        let result = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap()
            .into_result()
            .unwrap();
        assert_eq!(result.execution_result.rounds, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(p.requests.lock().unwrap().len(), 2);
        assert_eq!(p.dropped.load(Ordering::SeqCst), 0);
        assert_eq!(result.execution_result.usage.total_tokens, Some(14));
        assert_eq!(seen.lock().unwrap()[1].0, InputSource::Rpc);
        let requests = p.requests.lock().unwrap();
        assert!(
            requests[1]
                .messages
                .iter()
                .any(|m| m.text_content() == "EFFECTIVE_CONTROL")
        );
        assert!(
            !serde_json::to_string(&*requests)
                .unwrap()
                .contains("RAW_CONTROL_PRIVATE")
        );
        let saved = repo.open(&id).unwrap();
        let users = saved
            .messages()
            .iter()
            .filter(|m| m.message.role == MessageRole::User)
            .map(|m| m.message.text_content())
            .collect::<Vec<_>>();
        assert_eq!(users, ["initial", "EFFECTIVE_CONTROL"]);
        assert!(
            !serde_json::to_string(saved.entries())
                .unwrap()
                .contains("RAW_CONTROL_PRIVATE")
        );
        assert!(
            !serde_json::to_string(&*events.0.lock().unwrap())
                .unwrap()
                .contains("RAW_CONTROL_PRIVATE")
        );
    }
}
struct Wait {
    started: Arc<Notify>,
    release: Arc<Notify>,
    ended: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl InputHook for Wait {
    async fn input(
        &self,
        input: InputHookInput<'_>,
        c: ExtensionCancellation,
    ) -> Result<InputAction> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if prompt(input.input) == "wait" {
            let end = self.ended.clone();
            tokio::spawn(async move {
                c.cancelled().await;
                end.notify_one();
            });
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(InputAction::Continue)
    }
}
type WaitingFixture = (
    Arc<dyn InputHook>,
    Arc<Notify>,
    Arc<Notify>,
    Arc<Notify>,
    Arc<AtomicUsize>,
);
fn waiting() -> WaitingFixture {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let ended = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(Wait {
            started: started.clone(),
            release: release.clone(),
            ended: ended.clone(),
            calls: calls.clone(),
        }),
        started,
        release,
        ended,
        calls,
    )
}
#[tokio::test]
async fn dropped_input_futures_release_bounded_reservations_and_cancel_private_children() {
    let root = tempfile::tempdir().unwrap();
    let (h, started, _release, ended, calls) = waiting();
    let executor = executor(root.path(), Arc::new(Provider::default()), h);
    let (handle, _control) = executor
        .control_channel(
            TurnExecutionContext::new(),
            RunControlConfig {
                max_pending_inputs: 1,
                ..Default::default()
            },
        )
        .unwrap();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        task_handle
            .submit_input(RunInputKind::Steer, "wait".into())
            .await
    });
    notified(&started).await;
    let error = handle
        .submit_input(RunInputKind::Steer, "second".into())
        .await
        .unwrap_err();
    assert_eq!(
        error.error.downcast_ref::<RunControlError>(),
        Some(&RunControlError::QueueFull)
    );
    assert_eq!(error.input.message().text_content(), "second");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(handle.snapshot().pending_inputs.is_empty());
    task.abort();
    let _ = task.await;
    notified(&ended).await;
    assert!(matches!(
        handle
            .submit_input(RunInputKind::Steer, "third".into())
            .await
            .unwrap(),
        InputSubmission::Queued { .. }
    ));
}
#[tokio::test]
async fn input_processing_is_serial_and_completion_cannot_publish_late_queued_messages() {
    let root = tempfile::tempdir().unwrap();
    let (h, started, release, _ended, calls) = waiting();
    let executor = executor(root.path(), Arc::new(Provider::default()), h);
    let (handle, control) = executor
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let one = handle.clone();
    let first = tokio::spawn(async move {
        one.submit_input(RunInputKind::FollowUp, "wait".into())
            .await
    });
    notified(&started).await;
    let two = handle.clone();
    let second = tokio::spawn(async move {
        two.submit_input(RunInputKind::FollowUp, "second".into())
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release.notify_one();
    let a = first.await.unwrap().unwrap();
    let b = second.await.unwrap().unwrap();
    assert!(
        matches!((a,b),(InputSubmission::Queued{input_id:a},InputSubmission::Queued{input_id:b})if a<b)
    );
    assert_eq!(handle.snapshot().pending_inputs.len(), 2);
    drop(control);
    assert!(
        handle
            .submit_input(RunInputKind::Steer, "closed".into())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn closing_the_run_while_a_hook_waits_expires_the_callback_without_a_late_grant_or_queue_entry()
 {
    let root = tempfile::tempdir().unwrap();
    let (h, started, release, ended, _) = waiting();
    let executor = executor(root.path(), Arc::new(Provider::default()), h);
    let (handle, control) = executor
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let producer = handle.clone();
    let task = tokio::spawn(async move {
        producer
            .submit_input(RunInputKind::Steer, "wait".into())
            .await
    });
    notified(&started).await;
    drop(control);
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(
        error.error.downcast_ref::<RunControlError>(),
        Some(&RunControlError::Closed)
    );
    notified(&ended).await;
    release.notify_one();
    assert!(handle.snapshot().pending_inputs.is_empty());
}
#[tokio::test]
async fn input_failures_panics_and_oversized_transformations_never_fall_back_to_unprocessed_input()
{
    for mode in [Mode::Fail, Mode::Panic, Mode::Huge] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let (h, calls, _) = hook(mode);
        let agent = Agent::new(executor(root.path(), p.clone(), h), AgentConfig::new("m"));
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let outcome = agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "raw",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
        assert!(
            repo.open(&session.metadata().id)
                .unwrap()
                .entries()
                .is_empty()
        );
        assert!(p.requests.lock().unwrap().is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let error = format!("{outcome:?}");
        assert!(!error.contains("PRIVATE_NATIVE_ERROR"));
        assert!(!error.contains("PRIVATE_NATIVE_PANIC"));
    }
}
#[tokio::test]
async fn transformed_controls_recheck_message_and_complete_queue_budgets_and_keep_the_original_on_rejection()
 {
    let root = tempfile::tempdir().unwrap();
    let (h, _, _) = hook(Mode::TransformAll);
    let executor = executor(root.path(), Arc::new(Provider::default()), h);
    let (handle, _control) = executor
        .control_channel(
            TurnExecutionContext::new(),
            RunControlConfig {
                max_pending_bytes: 120,
                ..Default::default()
            },
        )
        .unwrap();
    let error = handle
        .submit_input(RunInputKind::Steer, "original".into())
        .await
        .unwrap_err();
    assert_eq!(
        error.error.downcast_ref::<RunControlError>(),
        Some(&RunControlError::QueueFull)
    );
    assert_eq!(error.input.message().text_content(), "original");
    assert!(handle.snapshot().pending_inputs.is_empty());
}
#[tokio::test]
async fn prequeued_raw_messages_cannot_bypass_a_later_installed_input_processor() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let (h, calls, _) = hook(Mode::HandleControls);
    let executor = executor(root.path(), p.clone(), h);
    let (handle, control) = RunHandle::pair();
    handle
        .steer(Message::text(MessageRole::User, "unprocessed"))
        .unwrap();
    let outcome = executor
        .run_controlled(
            ModelRequest::new("m", vec![Message::text(MessageRole::User, "initial")]),
            Arc::new(()),
            control,
        )
        .await;
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(p.requests.lock().unwrap().is_empty());
    assert_eq!(
        handle.snapshot().pending_inputs[0].message.text_content(),
        "unprocessed"
    );
}
struct FailJournal;
#[async_trait]
impl RunJournal for FailJournal {
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn record_input_hooks(&self, _: &InputHookRecord) -> Result<()> {
        Ok(())
    }
    async fn input_prepared(&self, _: usize, _: &RunInput, _: &InputReceipt) -> Result<()> {
        anyhow::bail!("prepared input refused")
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn failed_prepared_input_checkpoint_keeps_raw_sdk_provenance_without_exposing_it_in_serialized_events()
 {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let (h, _, _) = hook(Mode::TransformControls);
    let executor = executor(root.path(), p.clone(), h);
    let (handle, control) = executor
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    handle
        .submit_input(RunInputKind::Steer, "ORIGINAL_PENDING_PRIVATE".into())
        .await
        .unwrap();
    let outcome = executor
        .run_controlled_with_journal(
            ModelRequest::new("m", vec![Message::text(MessageRole::User, "initial")]),
            Arc::new(()),
            control,
            &FailJournal,
        )
        .await;
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    assert!(p.requests.lock().unwrap().is_empty());
    assert_eq!(
        outcome.progress().pending_inputs[0]
            .original_message()
            .text_content(),
        "ORIGINAL_PENDING_PRIVATE"
    );
    assert!(
        !serde_json::to_string(&outcome.progress().pending_inputs)
            .unwrap()
            .contains("ORIGINAL_PENDING_PRIVATE")
    );
}
#[tokio::test]
async fn prepared_input_is_executor_bound_and_is_not_reexecuted() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let (h, calls, _) = hook(Mode::TransformAll);
    let executor = executor(root.path(), p.clone(), h);
    let input = match executor
        .prepare_user_input(
            "raw".into(),
            InputDelivery::Initial,
            TurnExecutionContext::new(),
        )
        .await
        .unwrap()
    {
        InputPreparation::Ready(input) => input,
        _ => panic!(),
    };
    let other = TurnExecutor::builder(Provider::default())
        .workspace(root.path())
        .build()
        .unwrap();
    assert!(
        other
            .prepare_user_input(
                input.clone(),
                InputDelivery::Initial,
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    let agent = Agent::new(executor, AgentConfig::new("m"));
    agent.run(input, Arc::new(())).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn raw_input_receipts_admit_complete_output_before_session_writes() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let (h, _, _) = hook(Mode::TransformAll);
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
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
            .unwrap(),
        AgentConfig::new("m"),
    );
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "raw",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
    assert!(p.requests.lock().unwrap().is_empty());
    assert!(
        repo.open(&session.metadata().id)
            .unwrap()
            .entries()
            .is_empty()
    );
}

#[tokio::test]
async fn cached_input_preparation_retains_elapsed_budget_and_cannot_start_a_late_model_request() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let (h, calls, _) = hook(Mode::TransformAll);
    let executor = TurnExecutor::builder_from_shared(p.clone())
        .workspace(root.path())
        .extensions(extensions(h))
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_duration: Some(Duration::from_millis(300)),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let input = match executor
        .prepare_user_input(
            "raw".into(),
            InputDelivery::Initial,
            TurnExecutionContext::new(),
        )
        .await
        .unwrap()
    {
        InputPreparation::Ready(input) => input,
        _ => panic!(),
    };
    tokio::time::sleep(Duration::from_millis(350)).await;
    let outcome = Agent::new(executor, AgentConfig::new("m"))
        .run_outcome(input, Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(p.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn initial_input_timeout_and_cancellation_expire_private_children_without_persisting_a_turn()
{
    for cancel in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let (h, started, release, ended, _) = waiting();
        let mut e = Extension::new("wait", "1").unwrap();
        let mut spec = HookSpec::new("input");
        spec.timeout = Duration::from_millis(25);
        e.input_hook(spec, h).unwrap();
        let mut r = ExtensionRegistry::new();
        r.install(e).unwrap();
        let p = Arc::new(Provider::default());
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(p.clone())
                .workspace(root.path())
                .extensions(Arc::new(r.finish()))
                .build()
                .unwrap(),
            AgentConfig::new("m"),
        );
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let id = session.metadata().id.clone();
        let task_id = id.clone();
        let task_repo = repo.clone();
        let context = TurnExecutionContext::new();
        let worker = context.clone();
        let run = tokio::spawn(async move {
            agent
                .run_with_session_outcome(
                    &task_repo,
                    SessionSelection::Existing(task_id),
                    "wait",
                    Arc::new(()),
                    worker,
                )
                .await
        });
        notified(&started).await;
        if cancel {
            context.cancel("cancel input");
        }
        let outcome = run.await.unwrap();
        assert!(if cancel {
            matches!(outcome, AgentTurnOutcome::Aborted(_))
        } else {
            matches!(outcome, AgentTurnOutcome::Failed(_))
        });
        notified(&ended).await;
        release.notify_one();
        assert!(repo.open(&id).unwrap().entries().is_empty());
        assert!(p.requests.lock().unwrap().is_empty());
    }
}
struct ToolProvider(AtomicUsize);
#[async_trait]
impl ModelProvider for ToolProvider {
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
                            id: "leaf".into(),
                            name: "wait".into(),
                            arguments: serde_json::json!({}),
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
            usage: usage(7),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct WaitingTool {
    started: Arc<Notify>,
    release: Arc<Notify>,
    dropped: Arc<AtomicUsize>,
}
#[async_trait]
impl aicoder_core::tools::ExecutableTool for WaitingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "wait".into(),
            description: None,
            parameters: serde_json::json!({"type":"object"}),
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
        let mut guard = Attempt {
            done: false,
            dropped: self.dropped.clone(),
        };
        self.started.notify_one();
        self.release.notified().await;
        guard.done = true;
        Ok(aicoder_core::tools::ToolSuccess::new(serde_json::json!(
            "tool complete"
        )))
    }
}
#[tokio::test]
async fn handled_steering_does_not_cancel_an_active_tool_or_repeat_its_completed_effect() {
    let root = tempfile::tempdir().unwrap();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut tools = aicoder_core::tools::ToolRegistry::default();
    tools
        .register(WaitingTool {
            started: started.clone(),
            release: release.clone(),
            dropped: dropped.clone(),
        })
        .unwrap();
    let (h, calls, _) = hook(Mode::HandleControls);
    let provider = Arc::new(ToolProvider(AtomicUsize::new(0)));
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .registry(tools)
            .extensions(extensions(h))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let run = tokio::spawn(async move {
        agent
            .run_controlled("original", Arc::new(()), control)
            .await
    });
    notified(&started).await;
    assert!(matches!(
        handle
            .submit_input(RunInputKind::Steer, "status".into())
            .await
            .unwrap(),
        InputSubmission::Handled { .. }
    ));
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    release.notify_one();
    let result = run.await.unwrap().into_result().unwrap();
    assert_eq!(provider.0.load(Ordering::SeqCst), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert!(
        result
            .execution_result
            .messages
            .iter()
            .any(|m| m.tool_result().is_some_and(|(_, _, error)| !error))
    );
}

#[tokio::test]
async fn pre_owner_input_preparation_cannot_reset_the_parent_run_duration_budget() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let (h, calls, _) = hook(Mode::TransformControls);
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(h))
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_duration: Some(Duration::from_millis(300)),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let (handle, control) = agent
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    handle
        .submit_input(RunInputKind::Steer, "prepared before owner".into())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    let outcome = agent.run_controlled("initial", Arc::new(()), control).await;
    assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(p.requests.lock().unwrap().is_empty());
    assert_eq!(
        outcome.progress().pending_inputs[0]
            .original_message()
            .text_content(),
        "prepared before owner"
    );
}

#[tokio::test]
async fn eager_input_reservations_preserve_call_order_even_when_futures_are_polled_in_reverse() {
    let root = tempfile::tempdir().unwrap();
    let (h, calls, seen) = hook(Mode::TransformControls);
    let executor = executor(root.path(), Arc::new(Provider::default()), h);
    let (handle, _control) = executor
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let first = handle
        .begin_input(RunInputKind::FollowUp, "first".into())
        .unwrap();
    let second = handle
        .begin_input(RunInputKind::FollowUp, "second".into())
        .unwrap();
    let second = tokio::spawn(second);
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    first.await.unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(
        seen.lock()
            .unwrap()
            .iter()
            .map(|v| v.2.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    let snapshot = handle.snapshot();
    assert!(snapshot.pending_inputs[0].id < snapshot.pending_inputs[1].id);
}
#[tokio::test]
async fn dropped_unpolled_submission_does_not_run_callbacks_or_hold_the_next_reservation() {
    let root = tempfile::tempdir().unwrap();
    let (h, calls, _) = hook(Mode::TransformControls);
    let executor = executor(root.path(), Arc::new(Provider::default()), h);
    let (handle, _control) = executor
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let first = handle
        .begin_input(RunInputKind::Steer, "first".into())
        .unwrap();
    let second = handle
        .begin_input(RunInputKind::Steer, "second".into())
        .unwrap();
    drop(first);
    second.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(handle.snapshot().pending_inputs.len(), 1);
}
#[tokio::test]
async fn raw_credentials_removed_by_a_hook_remain_ephemeral_and_are_never_written_back_into_receipts()
 {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let (h, _, _) = hook(Mode::TransformAll);
    let executor = executor(root.path(), p.clone(), h);
    let input = match executor
        .prepare_user_input(
            "Authorization: Bearer SECRET_INPUT_TOKEN".into(),
            InputDelivery::Initial,
            TurnExecutionContext::new(),
        )
        .await
        .unwrap()
    {
        InputPreparation::Ready(input) => input,
        _ => panic!(),
    };
    let receipt = input.preparation_receipt().unwrap();
    assert!(
        receipt
            .original
            .as_ref()
            .unwrap()
            .text_content()
            .contains("SECRET_INPUT_TOKEN")
    );
    assert!(
        !serde_json::to_string(receipt)
            .unwrap()
            .contains("SECRET_INPUT_TOKEN")
    );
    let agent = Agent::new(executor, AgentConfig::new("m"));
    let repo = MemorySessionRepository::new();
    let saved = repo.create(root.path()).unwrap();
    agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(saved.metadata().id.clone()),
            input,
            Arc::new(()),
        )
        .await
        .unwrap();
    assert!(
        !serde_json::to_string(repo.open(&saved.metadata().id).unwrap().entries())
            .unwrap()
            .contains("SECRET_INPUT_TOKEN")
    );
}

#[tokio::test]
async fn waiting_for_an_unpolled_earlier_reservation_cannot_escape_the_parent_deadline() {
    let root = tempfile::tempdir().unwrap();
    let (h, calls, _) = hook(Mode::TransformControls);
    let executor = TurnExecutor::builder(Provider::default())
        .workspace(root.path())
        .extensions(extensions(h))
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_duration: Some(Duration::from_millis(50)),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let (handle, _control) = executor
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let first = handle
        .begin_input(RunInputKind::FollowUp, "first".into())
        .unwrap();
    let second = handle
        .begin_input(RunInputKind::FollowUp, "second".into())
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.error.downcast_ref::<BudgetExceeded>().is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(first);
    assert!(handle.snapshot().pending_inputs.is_empty());
}

struct Grow;
#[async_trait]
impl InputHook for Grow {
    async fn input(
        &self,
        input: InputHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<InputAction> {
        Ok(InputAction::Transform(
            input.input.clone().with_prompt_text("x".repeat(2048)),
        ))
    }
}
#[tokio::test]
async fn transformed_input_obeys_known_model_windows_before_session_mutation() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        window: Some(128),
        ..Default::default()
    });
    Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    )
    .run("small", Arc::new(()))
    .await
    .unwrap();
    let agent = Agent::new(
        executor(root.path(), p.clone(), Arc::new(Grow)),
        AgentConfig::new("m"),
    );
    let repo = MemorySessionRepository::new();
    let saved = repo.create(root.path()).unwrap();
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(saved.metadata().id.clone()),
            "small",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(!matches!(outcome, AgentTurnOutcome::Completed(_)));
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert!(
        repo.open(&saved.metadata().id)
            .unwrap()
            .entries()
            .is_empty()
    );
}
