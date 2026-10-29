use aicoder_core::session::*;
use aicoder_core::{model::*, tools::*, *};
use aicoder_runtime::session::JsonlSessionRepository;
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
#[derive(Clone)]
enum Step {
    Reply(ModelResponse),
    Fail,
    Pending,
}
struct Script {
    steps: Mutex<VecDeque<Step>>,
    requests: Mutex<Vec<ModelRequest>>,
    started: Arc<tokio::sync::Notify>,
}
impl Script {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps.into()),
            requests: Mutex::new(Vec::new()),
            started: Arc::new(tokio::sync::Notify::new()),
        })
    }
}
#[async_trait]
impl ModelProvider for Script {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("test", model);
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
        match step {
            Step::Reply(response) => {
                emit_response(&response, sink.as_ref());
                Ok(response)
            }
            Step::Fail => Err(ProviderError::new(
                "test",
                ProviderErrorKind::Authentication,
                "bad credentials",
            )),
            Step::Pending => {
                self.started.notify_one();
                std::future::pending().await
            }
        }
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
        usage: ModelUsage::default(),
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
struct ReadTool {
    name: &'static str,
    pending: bool,
    size: usize,
    done: Arc<AtomicUsize>,
    notify: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl ExecutableTool for ReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        if self.pending {
            return std::future::pending().await;
        }
        self.done.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_one();
        Ok(ToolSuccess::new(json!("x".repeat(self.size))))
    }
}
fn tools(
    names: &[(&'static str, bool, usize)],
    done: &Arc<AtomicUsize>,
    notify: &Arc<tokio::sync::Notify>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::default();
    for (name, pending, size) in names {
        registry
            .register(ReadTool {
                name,
                pending: *pending,
                size: *size,
                done: done.clone(),
                notify: notify.clone(),
            })
            .unwrap();
    }
    registry
}
#[derive(Default)]
struct Gate {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
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
        Ok(ToolSuccess::new(json!("changed")))
    }
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
#[tokio::test]
async fn session_checkpoints_survive_provider_failure_and_are_visible_before_terminal_events() {
    use aicoder_core::session::*;
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    for repository in [
        Arc::new(MemorySessionRepository::new()) as Arc<dyn SessionRepository>,
        Arc::new(JsonlSessionRepository::new(storage.path()).unwrap()),
    ] {
        let script = Script::new(vec![Step::Reply(calls(&["done"])), Step::Fail]);
        let done = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());
        let executor = TurnExecutor::builder_from_shared(script)
            .workspace(workspace.path())
            .registry(tools(&[("done", false, 4)], &done, &notify))
            .build()
            .unwrap();
        let agent = Agent::new(executor, AgentConfig::new("test"));
        let session = repository.create(workspace.path()).unwrap();
        let id = session.metadata().id.clone();
        let target = repository.clone();
        let expected = id.clone();
        let terminal = Arc::new(AtomicUsize::new(0));
        let observed = terminal.clone();
        let handler = Arc::new(move |event: &AgentRawEventEnvelope| {
            if matches!(event.event, AgentRawEvent::AgentFailed { .. }) {
                let session = target.open(&expected).unwrap();
                assert_eq!(session.chat_messages().len(), 3);
                assert!(
                    matches!(&session.active_entries().last().unwrap().data,SessionEntryData::TurnEnd {record,..} if record.stop.is_some())
                );
                observed.fetch_add(1, Ordering::SeqCst);
            }
        });
        let outcome = agent
            .run_with_session_outcome(
                repository.as_ref(),
                SessionSelection::Existing(id.clone()),
                "work",
                handler,
                TurnExecutionContext::new(),
            )
            .await;
        assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
        assert_eq!(done.load(Ordering::SeqCst), 1);
        assert_eq!(terminal.load(Ordering::SeqCst), 1);
        let saved = repository.open(&id).unwrap();
        assert_eq!(saved.chat_messages().len(), 3);
        assert!(
            saved
                .model_messages()
                .unwrap()
                .iter()
                .any(|m| m.role == MessageRole::System)
        );
        assert!(saved.unfinished_turn().is_none());
        // A subsequent turn consumes recorded results without executing the operation again.
        let resumed_script = Script::new(vec![Step::Reply(reply("recovered"))]);
        let resumed = Agent::new(
            TurnExecutor::builder_from_shared(resumed_script.clone())
                .workspace(workspace.path())
                .registry(ToolRegistry::default())
                .build()
                .unwrap(),
            AgentConfig::new("test"),
        );
        resumed
            .run_with_session(
                repository.as_ref(),
                SessionSelection::Existing(id),
                "continue",
                Arc::new(()),
            )
            .await
            .unwrap();
        assert_eq!(done.load(Ordering::SeqCst), 1);
        let requests = resumed_script.requests.lock().unwrap();
        assert!(
            requests[0]
                .messages
                .iter()
                .any(|m| m.tool_result().is_some())
        );
    }
}

#[tokio::test]
async fn dropping_a_running_task_keeps_checkpoints_and_releases_session_ownership() {
    use aicoder_core::session::*;
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let repository = Arc::new(JsonlSessionRepository::new(storage.path()).unwrap());
    let session = repository.create(workspace.path()).unwrap();
    let id = session.metadata().id.clone();
    let script = Script::new(vec![Step::Reply(calls(&["done"])), Step::Pending]);
    let started = script.started.clone();
    let done = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder_from_shared(script)
        .workspace(workspace.path())
        .registry(tools(&[("done", false, 4)], &done, &notify))
        .build()
        .unwrap();
    let agent = Agent::new(executor, AgentConfig::new("test"));
    let target = repository.clone();
    let selection = SessionSelection::Existing(id.clone());
    let run = tokio::spawn(async move {
        agent
            .run_with_session(target.as_ref(), selection, "work", Arc::new(()))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    let mut reader = repository.open(&id).unwrap();
    assert_eq!(reader.messages().len(), 3);
    assert!(repository.acquire_run(&mut reader).is_err());
    assert!(repository.delete(&id).is_err());
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    let saved = repository.open(&id).unwrap();
    assert!(saved.unfinished_turn().is_some());
    assert_eq!(saved.chat_messages().len(), 3);
    assert_eq!(done.load(Ordering::SeqCst), 1);
    let script = Script::new(vec![Step::Reply(reply("continued"))]);
    let executor = TurnExecutor::builder_from_shared(script.clone())
        .workspace(workspace.path())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
    Agent::new(executor, AgentConfig::new("test"))
        .run_with_session(
            repository.as_ref(),
            SessionSelection::Existing(id.clone()),
            "inspect then continue",
            Arc::new(()),
        )
        .await
        .unwrap();
    let session = repository.open(&id).unwrap();
    assert!(session.unfinished_turn().is_none());
    assert!(session.entries().iter().any(|e|matches!(&e.data,SessionEntryData::TurnEnd {record,..} if record.statistics.is_none() && record.stop.is_some())));
    let requests = script.requests.lock().unwrap();
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.role == MessageRole::System)
    );
}

#[tokio::test]
async fn session_cancellation_preserves_a_later_parallel_result_and_repairs_only_the_missing_call()
{
    use aicoder_core::session::*;
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    for repository in [
        Arc::new(MemorySessionRepository::new()) as Arc<dyn SessionRepository>,
        Arc::new(JsonlSessionRepository::new(storage.path()).unwrap()),
    ] {
        let script = Script::new(vec![Step::Reply(calls(&["pending", "done"]))]);
        let done = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());
        let executor = TurnExecutor::builder_from_shared(script)
            .workspace(workspace.path())
            .registry(tools(
                &[("pending", true, 0), ("done", false, 4)],
                &done,
                &notify,
            ))
            .build()
            .unwrap();
        let agent = Agent::new(executor, AgentConfig::new("test"));
        let session = repository.create(workspace.path()).unwrap();
        let id = session.metadata().id.clone();
        let target = repository.clone();
        let selection = SessionSelection::Existing(id.clone());
        let context = TurnExecutionContext::new();
        let cancel = context.clone();
        let run = tokio::spawn(async move {
            agent
                .run_with_session_outcome(target.as_ref(), selection, "work", Arc::new(()), context)
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .unwrap();
        cancel.cancel("stop parallel batch");
        let outcome = run.await.unwrap();
        assert!(
            matches!(&outcome,AgentTurnOutcome::Aborted(turn) if turn.persistence_error.is_none())
        );
        let saved = repository.open(&id).unwrap();
        assert_eq!(saved.chat_messages().len(), 3);
        assert_eq!(saved.chat_messages()[2].tool_result().unwrap().0, "c1");
        assert!(saved.unfinished_turn().is_none());
        let context = saved.model_messages().unwrap();
        assert_eq!(context[2].tool_result().unwrap().0, "c0");
        assert!(context[2].tool_result().unwrap().2);
        assert_eq!(context[3].tool_result().unwrap().0, "c1");
        assert!(!context[3].tool_result().unwrap().2);
        assert_eq!(done.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn steering_waits_for_the_active_mutation_then_skips_unstarted_tools_and_persists_provenance()
{
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    for store in [
        Arc::new(MemorySessionRepository::new()) as Arc<dyn SessionRepository>,
        Arc::new(JsonlSessionRepository::new(storage.path()).unwrap()),
    ] {
        let script = Script::new(vec![
            Step::Reply(calls(&["first", "second"])),
            Step::Reply(reply("redirected")),
        ]);
        let gate = Arc::new(Gate::default());
        let executed = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::default();
        registry
            .register(CommandTool {
                name: "first",
                gate: Some(gate.clone()),
                executed: executed.clone(),
            })
            .unwrap();
        registry
            .register(CommandTool {
                name: "second",
                gate: None,
                executed: executed.clone(),
            })
            .unwrap();
        let agent = Agent::new(
            executor(script.clone(), registry, workspace.path()),
            AgentConfig::new("test"),
        );
        let (handle, control) = RunHandle::pair();
        let target = store.clone();
        let run = tokio::spawn(async move {
            agent
                .run_with_session_controlled(
                    target.as_ref(),
                    SessionSelection::New,
                    "initial",
                    Arc::new(()),
                    control,
                )
                .await
        });
        gate.wait().await;
        let input = handle.steer(user("change direction")).unwrap();
        assert_eq!(executed.load(Ordering::SeqCst), 0);
        gate.release.notify_one();
        let outcome = run.await.unwrap();
        assert!(
            matches!(outcome, AgentTurnOutcome::Completed(_)),
            "{outcome:?}"
        );
        assert_eq!(executed.load(Ordering::SeqCst), 1);
        let id = outcome.session().unwrap().id.clone();
        assert_eq!(handle.snapshot().session_id, Some(id.clone()));
        let session = store.open(&id).unwrap();
        assert!(session.entries().iter().any(|entry|matches!(&entry.data,SessionEntryData::Extension {namespace,data} if namespace=="aicoder.run.input" && data["id"]==input && data.get("message").is_none())));
        assert_eq!(session.model_messages().unwrap().len(), 6);
        let requests = script.requests.lock().unwrap();
        let messages = &requests[1].messages;
        assert_eq!(messages[2].tool_result().unwrap().0, "c0");
        assert!(!messages[2].tool_result().unwrap().2);
        assert_eq!(
            messages[3].tool_result().unwrap().1["error"]["code"],
            "steered_before_execution"
        );
        assert_eq!(messages.last(), Some(&user("change direction")));
    }
}

#[tokio::test]
async fn persisted_follow_ups_keep_answers_and_inputs_in_order_without_duplicate_checkpoints() {
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    for store in [
        Arc::new(MemorySessionRepository::new()) as Arc<dyn SessionRepository>,
        Arc::new(JsonlSessionRepository::new(storage.path()).unwrap()),
    ] {
        let script = Script::new(vec![
            Step::Reply(reply("one")),
            Step::Reply(reply("two")),
            Step::Reply(reply("three")),
        ]);
        let agent = Agent::new(
            executor(script, ToolRegistry::default(), workspace.path()),
            AgentConfig::new("test"),
        );
        let (handle, control) = RunHandle::pair();
        handle.follow_up(user("follow one")).unwrap();
        handle.follow_up(user("follow two")).unwrap();
        let outcome = agent
            .run_with_session_controlled(
                store.as_ref(),
                SessionSelection::New,
                "initial",
                Arc::new(()),
                control,
            )
            .await;
        assert!(
            matches!(&outcome, AgentTurnOutcome::Completed(_)),
            "{outcome:?}"
        );
        let session = store.open(&outcome.session().unwrap().id).unwrap();
        assert_eq!(
            session.chat_messages(),
            vec![
                user("initial"),
                Message::text(MessageRole::Assistant, "one"),
                user("follow one"),
                Message::text(MessageRole::Assistant, "two"),
                user("follow two"),
                Message::text(MessageRole::Assistant, "three")
            ]
        );
        assert_eq!(session.model_messages().unwrap(), session.chat_messages());
        assert_eq!(session.entries().iter().filter(|entry|matches!(&entry.data,SessionEntryData::Extension {namespace,..} if namespace=="aicoder.run.input")).count(),2);
        assert!(
            matches!(&session.active_entries().last().unwrap().data,SessionEntryData::TurnEnd {record,..} if record.pending_inputs.is_empty() && record.statistics.as_ref().unwrap().rounds==3)
        );
    }
}
