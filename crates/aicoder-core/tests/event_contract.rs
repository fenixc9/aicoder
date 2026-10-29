use aicoder_core::{events::*, model::*, session::*, tools::ToolRegistry, *};
use async_trait::async_trait;
use serde_json::json;
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn emitter() -> AgentEventEmitter {
    AgentEventEmitter::new(Arc::new(()))
}
fn chunk(text: &str) -> AgentRawEvent {
    AgentRawEvent::ContentChunk {
        response_index: 0,
        delta: text.into(),
    }
}
async fn drain(
    stream: &mut EventSubscription,
) -> Result<Vec<Arc<AgentRawEventEnvelope>>, EventStreamError> {
    let mut events = vec![];
    while let Some(event) = stream.recv().await? {
        events.push(event);
    }
    Ok(events)
}
fn validate(events: &[Arc<AgentRawEventEnvelope>]) {
    let mut validator = EventTraceValidator::new();
    for event in events {
        let encoded = serde_json::to_vec(event.as_ref()).unwrap();
        let decoded: AgentRawEventEnvelope = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            serde_json::to_value(event.as_ref()).unwrap(),
            serde_json::to_value(&decoded).unwrap()
        );
        validator.observe(&decoded).unwrap();
    }
    validator.finish().unwrap();
}
struct StaticModel;
#[async_trait]
impl ModelProvider for StaticModel {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        Ok(ModelSpec::new("offline", model))
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
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
fn executor() -> TurnExecutor {
    TurnExecutor::builder(StaticModel)
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .build()
        .unwrap()
}
fn request() -> ModelRequest {
    ModelRequest::new("offline", vec![Message::text(MessageRole::User, "hello")])
}

#[tokio::test]
async fn count_overflow_retains_a_contiguous_prefix_then_returns_an_explicit_error_once() {
    let events = emitter();
    let mut stream = events
        .source()
        .subscribe(EventBufferConfig {
            max_events: 2,
            max_bytes: 4096,
        })
        .unwrap();
    for _ in 0..3 {
        events.emit(chunk("delta"));
    }
    assert_eq!(stream.report().peak_events, 2);
    assert_eq!(stream.recv().await.unwrap().unwrap().sequence, 1);
    assert_eq!(stream.recv().await.unwrap().unwrap().sequence, 2);
    let error = stream.recv().await.unwrap_err();
    assert_eq!(error.status, EventDeliveryStatus::Lagged);
    assert_eq!(error.first_missing_sequence, Some(3));
    assert_eq!(error.last_delivered_sequence, Some(2));
    assert!(stream.recv().await.unwrap().is_none());
    assert!(events.shutdown().await.has_failures());
}
#[tokio::test]
async fn serialized_byte_capacity_is_independent_of_event_count_and_is_released_on_receive() {
    let events = emitter();
    let mut stream = events
        .source()
        .subscribe(EventBufferConfig {
            max_events: 100,
            max_bytes: 512,
        })
        .unwrap();
    events.emit(chunk(&"x".repeat(100)));
    let first = stream.recv().await.unwrap().unwrap();
    assert!(serde_json::to_vec(first.as_ref()).unwrap().len() < 512);
    assert_eq!(stream.report().queued_bytes, 0);
    for _ in 0..10 {
        events.emit(chunk(&"x".repeat(100)));
    }
    assert_eq!(stream.report().status, EventDeliveryStatus::Lagged);
    assert!(stream.report().peak_events < 100);
    assert!(stream.report().peak_bytes <= 512);
    assert!(drain(&mut stream).await.is_err());
    events.shutdown().await;
}
#[tokio::test]
async fn a_single_oversized_frame_is_rejected_without_truncating_its_payload() {
    let events = emitter();
    let mut stream = events
        .source()
        .subscribe(EventBufferConfig {
            max_events: 10,
            max_bytes: 256,
        })
        .unwrap();
    events.emit(chunk(&"大".repeat(1024)));
    let error = stream.recv().await.unwrap_err();
    assert_eq!(error.status, EventDeliveryStatus::EventTooLarge);
    assert_eq!(error.first_missing_sequence, Some(1));
    assert_eq!(stream.report().peak_bytes, 0);
    assert_eq!(stream.report().queued_events, 0);
    events.shutdown().await;
}
#[tokio::test]
async fn slow_subscribers_do_not_affect_fast_subscribers_or_cancel_the_run() {
    let events = emitter();
    let mut slow = events
        .source()
        .subscribe(EventBufferConfig {
            max_events: 1,
            max_bytes: 4096,
        })
        .unwrap();
    let mut fast = events
        .source()
        .subscribe(EventBufferConfig::default())
        .unwrap();
    for sequence in 1..=5 {
        events.emit(chunk("hello"));
        assert_eq!(fast.recv().await.unwrap().unwrap().sequence, sequence);
    }
    events.shutdown().await;
    assert!(fast.recv().await.unwrap().is_none());
    assert_eq!(fast.report().status, EventDeliveryStatus::Completed);
    assert_eq!(slow.recv().await.unwrap().unwrap().sequence, 1);
    assert_eq!(
        slow.recv().await.unwrap_err().status,
        EventDeliveryStatus::Lagged
    );
    let (handle, control) = RunHandle::pair();
    let mut stream = handle
        .subscribe(EventBufferConfig {
            max_events: 1,
            max_bytes: 4096,
        })
        .unwrap();
    let outcome = executor()
        .run_controlled(request(), Arc::new(()), control)
        .await;
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert!(outcome.progress().event_delivery.has_failures());
    assert!(drain(&mut stream).await.is_err());
}
#[tokio::test]
async fn disconnect_is_explicit_and_does_not_change_cancellation_or_result() {
    let (handle, control) = RunHandle::pair();
    let subscription = handle.subscribe(EventBufferConfig::default()).unwrap();
    drop(subscription);
    let outcome = executor()
        .run_controlled(request(), Arc::new(()), control)
        .await;
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(
        outcome.progress().event_delivery.observers[0].status,
        EventDeliveryStatus::ClientClosed
    );
    assert_eq!(handle.snapshot().status, RunControlStatus::Finished);
}
#[tokio::test]
async fn concurrent_producers_have_one_ordered_sequence_and_clones_do_not_own_shutdown() {
    let events = emitter();
    let mut stream = events
        .source()
        .subscribe(EventBufferConfig {
            max_events: 1000,
            max_bytes: 1024 * 1024,
        })
        .unwrap();
    let producers = (0..8)
        .map(|_| {
            let producer = events.clone();
            std::thread::spawn(move || {
                for _ in 0..40 {
                    producer.emit(chunk("parallel"));
                }
            })
        })
        .collect::<Vec<_>>();
    for producer in producers {
        producer.join().unwrap();
    }
    events.shutdown().await;
    let received = drain(&mut stream).await.unwrap();
    assert_eq!(received.len(), 320);
    assert!(
        received
            .iter()
            .enumerate()
            .all(|(index, event)| event.sequence == index as u64 + 1)
    );
}
#[tokio::test]
async fn owner_drop_reports_abandonment_even_when_a_producer_clone_survives() {
    let events = emitter();
    let late = events.clone();
    let mut stream = events
        .source()
        .subscribe(EventBufferConfig::default())
        .unwrap();
    events.emit(chunk("queued"));
    drop(events);
    assert_eq!(
        stream.recv().await.unwrap_err().status,
        EventDeliveryStatus::Abandoned
    );
    assert!(!late.try_emit(chunk("too late")));
    assert!(stream.recv().await.unwrap().is_none());
}
#[tokio::test]
async fn terminal_publication_seals_all_producers_and_new_subscriptions() {
    let events = emitter();
    let mut stream = events
        .source()
        .subscribe(EventBufferConfig::default())
        .unwrap();
    events.emit(AgentRawEvent::AgentCompleted {
        rounds: 0,
        usage: Arc::new(ModelUsage::zero()),
    });
    assert!(!events.try_emit(chunk("late")));
    assert!(matches!(
        events.source().subscribe(EventBufferConfig::default()),
        Err(SubscribeError::RunClosed)
    ));
    assert!(stream.recv().await.unwrap().unwrap().event.is_terminal());
    assert!(stream.recv().await.unwrap().is_none());
    events.shutdown().await;
}
#[test]
fn subscription_registration_is_bounded_even_if_clients_repeatedly_disconnect() {
    let events = emitter();
    assert!(matches!(
        events.source().subscribe(EventBufferConfig {
            max_events: 0,
            max_bytes: 1
        }),
        Err(SubscribeError::InvalidLimits)
    ));
    for _ in 0..MAX_EVENT_SUBSCRIPTIONS {
        drop(
            events
                .source()
                .subscribe(EventBufferConfig::default())
                .unwrap(),
        );
    }
    assert!(matches!(
        events.source().subscribe(EventBufferConfig::default()),
        Err(SubscribeError::TooManySubscriptions)
    ));
}
#[tokio::test]
async fn a_complete_core_run_round_trips_as_versioned_json_with_a_terminal_summary() {
    let (handle, control) = RunHandle::pair();
    let mut stream = handle.subscribe(EventBufferConfig::default()).unwrap();
    let outcome = executor()
        .run_controlled(request(), Arc::new(()), control)
        .await;
    let events = drain(&mut stream).await.unwrap();
    validate(&events);
    assert!(events.iter().all(|event| event.run_id == handle.run_id()
        && event.schema_version == EVENT_SCHEMA_VERSION
        && event.session_id.is_none()));
    let summary = events.last().unwrap().termination.as_ref().unwrap();
    assert!(summary.stop.is_none());
    assert_eq!(summary.rounds, outcome.progress().rounds);
    assert_eq!(summary.usage, outcome.progress().usage);
    assert_eq!(
        outcome.progress().event_delivery.observers[0].status,
        EventDeliveryStatus::Draining
    );
    assert_eq!(
        handle.events().report().observers[0].status,
        EventDeliveryStatus::Completed
    );
    let mut encoded = serde_json::to_value(events[0].as_ref()).unwrap();
    encoded["schema_version"] = json!(999);
    assert!(serde_json::from_value::<AgentRawEventEnvelope>(encoded).is_err());
}
#[tokio::test]
async fn session_and_turn_identity_are_bound_once_and_match_the_persisted_attempt() {
    let workspace = tempfile::tempdir().unwrap();
    let store = MemorySessionRepository::new();
    let agent = Agent::new(
        TurnExecutor::builder(StaticModel)
            .workspace(workspace.path())
            .registry(ToolRegistry::default())
            .build()
            .unwrap(),
        AgentConfig::new("offline"),
    );
    let (handle, control) = RunHandle::pair();
    let mut stream = handle.subscribe(EventBufferConfig::default()).unwrap();
    let outcome = agent
        .run_with_session_controlled(
            &store,
            SessionSelection::New,
            "hello",
            Arc::new(()),
            control,
        )
        .await;
    let events = drain(&mut stream).await.unwrap();
    validate(&events);
    let session = store.open(&outcome.session().unwrap().id).unwrap();
    let turn = session
        .entries()
        .iter()
        .find_map(|entry| {
            if let SessionEntryData::TurnStart { turn_id } = &entry.data {
                Some(turn_id)
            } else {
                None
            }
        })
        .unwrap();
    let bound = events
        .iter()
        .position(|event| matches!(event.event, AgentRawEvent::SessionBound { .. }))
        .unwrap();
    assert!(
        events[..bound]
            .iter()
            .all(|event| event.session_id.is_none())
    );
    assert!(
        events[bound..]
            .iter()
            .all(
                |event| event.session_id.as_deref() == Some(session.metadata().id.as_str())
                    && event.turn_id.as_deref() == Some(turn.as_str())
            )
    );
}
#[tokio::test]
async fn session_preparation_failure_and_pre_cancellation_have_complete_lifecycles() {
    let workspace = tempfile::tempdir().unwrap();
    let store = MemorySessionRepository::new();
    let agent = Agent::new(
        TurnExecutor::builder(StaticModel)
            .workspace(workspace.path())
            .registry(ToolRegistry::default())
            .build()
            .unwrap(),
        AgentConfig::new("offline"),
    );
    for cancel in [false, true] {
        let (handle, control) = RunHandle::pair();
        let mut stream = handle.subscribe(EventBufferConfig::default()).unwrap();
        if cancel {
            handle.cancel("before preparation").unwrap();
        }
        let outcome = agent
            .run_with_session_controlled(
                &store,
                SessionSelection::Existing("missing".into()),
                "work",
                Arc::new(()),
                control,
            )
            .await;
        assert!(!matches!(outcome, AgentTurnOutcome::Completed(_)));
        let events = drain(&mut stream).await.unwrap();
        validate(&events);
        assert!(
            events
                .last()
                .unwrap()
                .termination
                .as_ref()
                .unwrap()
                .stop
                .is_some()
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.event, AgentRawEvent::ModelRequestStarted))
        );
    }
}
#[tokio::test]
async fn a_round_stopped_before_a_model_request_is_closed_before_the_terminal_event() {
    let (handle, control) = RunHandle::pair();
    let mut stream = handle.subscribe(EventBufferConfig::default()).unwrap();
    let executor = TurnExecutor::builder(StaticModel)
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_tokens: Some(0),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let outcome = executor
        .run_controlled(request(), Arc::new(()), control)
        .await;
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    let events = drain(&mut stream).await.unwrap();
    validate(&events);
    assert!(events.iter().any(|event| matches!(
        event.event,
        AgentRawEvent::RoundCompleted {
            outcome: RoundOutcome::Stopped { .. }
        }
    )));
}
struct Blocker {
    gate: Arc<(Mutex<bool>, Condvar)>,
    started: Arc<tokio::sync::Notify>,
    count: Arc<AtomicUsize>,
}
impl AgentEventHandler for Blocker {
    fn on_raw_event(&self, _: &AgentRawEventEnvelope) {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        let (lock, ready) = self.gate.as_ref();
        let mut released = lock.lock().unwrap();
        while !*released {
            released = ready.wait(released).unwrap();
        }
    }
}
struct Release(Arc<(Mutex<bool>, Condvar)>);
impl Drop for Release {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}
#[tokio::test]
async fn a_blocked_callback_has_a_bounded_drain_and_does_not_block_other_observers() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Release(gate.clone());
    let started = Arc::new(tokio::sync::Notify::new());
    let count = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(Blocker {
        gate,
        started: started.clone(),
        count: count.clone(),
    });
    let worker_handler = Arc::downgrade(&handler);
    let (handle, control) = RunHandle::pair();
    let mut stream = handle.subscribe(EventBufferConfig::default()).unwrap();
    let executor = TurnExecutor::builder(StaticModel)
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .config(TurnExecutionConfig {
            events: EventDeliveryConfig {
                callback_drain_timeout: Duration::from_millis(50),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let run =
        tokio::spawn(async move { executor.run_controlled(request(), handler, control).await });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let report = outcome
        .progress()
        .event_delivery
        .observers
        .iter()
        .find(|report| report.callback)
        .unwrap();
    assert_eq!(report.status, EventDeliveryStatus::CallbackTimedOut);
    assert!(report.queued_events == 0 && report.queued_bytes == 0);
    validate(&drain(&mut stream).await.unwrap());
    drop(release);
    tokio::time::timeout(Duration::from_secs(2), async {
        while worker_handler.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let report = handle
        .events()
        .report()
        .observers
        .into_iter()
        .find(|report| report.callback)
        .unwrap();
    assert_eq!(report.status, EventDeliveryStatus::CallbackTimedOut);
    assert_eq!(report.last_delivered_sequence, None);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn a_panicking_callback_is_detached_and_reported_without_replacing_the_run_result() {
    let handler = Arc::new(|_: &AgentRawEventEnvelope| panic!("test observer panic"));
    let outcome = executor()
        .run_outcome(request(), handler, TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert!(
        outcome
            .progress()
            .event_delivery
            .observers
            .iter()
            .any(|report| report.status == EventDeliveryStatus::HandlerPanicked)
    );
}

#[tokio::test]
async fn the_published_v1_fixture_matches_actual_sdk_output_after_identity_and_time_normalization()
{
    let expected = include_str!("../../../docs/protocol/events-v1-trace.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let mut validator = EventTraceValidator::new();
    for record in &expected {
        let event = serde_json::from_value(record.clone()).unwrap();
        validator.observe(&event).unwrap();
    }
    validator.finish().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(StaticModel)
            .workspace(workspace.path())
            .registry(ToolRegistry::default())
            .build()
            .unwrap(),
        AgentConfig::new("offline"),
    );
    let (handle, control) = RunHandle::pair();
    let mut stream = handle.subscribe(EventBufferConfig::default()).unwrap();
    agent
        .run_with_session_controlled(
            &MemorySessionRepository::new(),
            SessionSelection::New,
            "hello",
            Arc::new(()),
            control,
        )
        .await
        .into_result()
        .unwrap();
    let mut actual = drain(&mut stream)
        .await
        .unwrap()
        .iter()
        .map(|record| serde_json::to_value(record.as_ref()).unwrap())
        .collect::<Vec<_>>();
    for record in &mut actual {
        record["run_id"] = json!("00000000-0000-4000-8000-000000000001");
        record["elapsed_ms"] = json!(0);
        if record.get("session_id").is_some() {
            record["session_id"] = json!("00000000-0000-4000-8000-000000000002");
            record["turn_id"] = json!("00000000-0000-4000-8000-000000000003");
        }
        if record["event"]["type"] == "session_bound" {
            record["event"]["session_id"] = json!("00000000-0000-4000-8000-000000000002");
            record["event"]["turn_id"] = json!("00000000-0000-4000-8000-000000000003");
        }
        if record.get("termination").is_some() {
            record["termination"]["elapsed_ms"] = json!(0);
            if let Some(source) = record["termination"]["model_context"]["source"].as_object_mut() {
                source.insert(
                    "session_id".into(),
                    json!("00000000-0000-4000-8000-000000000002"),
                );
                source.insert(
                    "leaf_id".into(),
                    json!("00000000-0000-4000-8000-000000000004"),
                );
            }
        }
    }
    assert_eq!(actual, expected);
}
#[test]
fn invalid_sequences_identity_changes_missing_summaries_and_late_events_cannot_pass_trace_validation()
 {
    let records = include_str!("../../../docs/protocol/events-v1-trace.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<AgentRawEventEnvelope>(line).unwrap())
        .collect::<Vec<_>>();
    for mutation in 0..4 {
        let mut changed = records.clone();
        match mutation {
            0 => changed[1].sequence += 1,
            1 => changed[1].run_id = RunId::new(),
            2 => changed.last_mut().unwrap().termination = None,
            3 => changed.push(changed.last().unwrap().clone()),
            _ => unreachable!(),
        }
        let mut validator = EventTraceValidator::new();
        assert!(
            changed
                .iter()
                .try_for_each(|event| validator.observe(event))
                .is_err()
        );
        assert!(validator.finish().is_err());
        assert!(validator.observe(&records[0]).is_err());
    }
    let mut unknown = serde_json::to_value(&records[0]).unwrap();
    unknown["event"]["type"] = json!("unknown_event");
    assert!(serde_json::from_value::<AgentRawEventEnvelope>(unknown).is_err());
    let mut zero = serde_json::to_value(&records[0]).unwrap();
    zero["sequence"] = json!(0);
    assert!(serde_json::from_value::<AgentRawEventEnvelope>(zero).is_err());
}
struct PendingModel(Arc<tokio::sync::Notify>);
#[async_trait]
impl ModelProvider for PendingModel {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        Ok(ModelSpec::new("offline", model))
    }
    async fn generate(
        &self,
        _: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        sink.emit(ModelEvent::Started);
        sink.emit(ModelEvent::TextDelta {
            delta: "partial".into(),
        });
        self.0.notify_one();
        std::future::pending().await
    }
}
#[tokio::test]
async fn cancellation_closes_partial_stream_and_round_before_one_authoritative_terminal_event() {
    let started = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder(PendingModel(started.clone()))
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
    let (handle, control) = RunHandle::pair();
    let mut stream = handle.subscribe(EventBufferConfig::default()).unwrap();
    let run = tokio::spawn(async move {
        executor
            .run_controlled(request(), Arc::new(()), control)
            .await
    });
    started.notified().await;
    handle.cancel("stop stream").unwrap();
    let outcome = run.await.unwrap();
    assert!(
        matches!(&outcome,RunOutcome::Interrupted(turn) if turn.progress.partial_response.is_some())
    );
    let records = drain(&mut stream).await.unwrap();
    validate(&records);
    assert_eq!(
        records
            .iter()
            .filter(|event| event.event.is_terminal())
            .count(),
        1
    );
    assert!(records.iter().any(|event| matches!(
        &event.event,
        AgentRawEvent::ContentEnded {
            outcome: StreamEnd::Aborted { .. },
            ..
        }
    )));
}
#[tokio::test]
async fn task_destruction_ends_observation_with_an_error_and_no_invented_terminal_record() {
    let started = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder(PendingModel(started.clone()))
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
    let (handle, control) = RunHandle::pair();
    let mut stream = handle.subscribe(EventBufferConfig::default()).unwrap();
    let run = tokio::spawn(async move {
        executor
            .run_controlled(request(), Arc::new(()), control)
            .await
    });
    started.notified().await;
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    assert_eq!(
        stream.recv().await.unwrap_err().status,
        EventDeliveryStatus::Abandoned
    );
    assert_eq!(handle.snapshot().status, RunControlStatus::Dropped);
}

struct FailFinish;
#[async_trait]
impl RunJournal for FailFinish {
    async fn checkpoint(&self, _: usize, _: &Message) -> anyhow::Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> anyhow::Result<()> {
        anyhow::bail!("test journal unavailable")
    }
}
#[tokio::test]
async fn terminal_metadata_reports_primary_stop_pending_input_ids_and_secondary_persistence_failure()
 {
    let started = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder(PendingModel(started.clone()))
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
    let (handle, control) = RunHandle::pair();
    let id = handle
        .follow_up(Message::text(
            MessageRole::User,
            "queued body stays in progress",
        ))
        .unwrap();
    let mut stream = handle.subscribe(EventBufferConfig::default()).unwrap();
    let run = tokio::spawn(async move {
        executor
            .run_controlled_with_journal(request(), Arc::new(()), control, &FailFinish)
            .await
    });
    started.notified().await;
    handle.cancel("host stop").unwrap();
    let outcome = run.await.unwrap();
    let records = drain(&mut stream).await.unwrap();
    validate(&records);
    let summary = records.last().unwrap().termination.as_ref().unwrap();
    assert!(matches!(&summary.stop,Some(RunStop::Cancelled {reason}) if reason=="host stop"));
    assert!(summary.persistence_error.is_some());
    assert_eq!(summary.pending_inputs[0].id, id);
    assert_eq!(summary.pending_inputs[0].kind, RunInputKind::FollowUp);
    assert_eq!(outcome.progress().pending_inputs[0].id, id);
    assert!(
        !serde_json::to_string(summary)
            .unwrap()
            .contains("queued body stays in progress")
    );
}
#[tokio::test]
async fn callback_byte_overflow_is_observable_even_if_no_frame_fits_in_the_buffer() {
    let recorder =
        Arc::new(|_: &AgentRawEventEnvelope| panic!("no frame should reach this callback"));
    let executor = TurnExecutor::builder(StaticModel)
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .config(TurnExecutionConfig {
            events: EventDeliveryConfig {
                callback_buffer: EventBufferConfig {
                    max_events: 10,
                    max_bytes: 1,
                },
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let outcome = executor
        .run_outcome(request(), recorder, TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    let report = &outcome.progress().event_delivery.observers[0];
    assert_eq!(report.status, EventDeliveryStatus::EventTooLarge);
    assert_eq!(report.first_missing_sequence, Some(1));
    assert_eq!(report.last_delivered_sequence, None);
}

#[tokio::test(start_paused = true)]
async fn virtual_execution_time_does_not_expire_a_healthy_wall_clock_callback_drain() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let target = received.clone();
    let handler = Arc::new(move |event: &AgentRawEventEnvelope| {
        std::thread::sleep(Duration::from_millis(2));
        target.lock().unwrap().push(event.clone());
    });
    let outcome = executor()
        .run_outcome(request(), handler, TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert!(!outcome.progress().event_delivery.has_failures());
    let records = received
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .map(Arc::new)
        .collect::<Vec<_>>();
    validate(&records);
}
