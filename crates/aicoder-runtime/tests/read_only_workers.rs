#[path = "../examples/support/read_only_workers.rs"]
mod read_only_workers;
use aicoder_core::{extensions::*, model::*, session::*, tools::*, *};
use aicoder_runtime::tools::{ReadFileTool, WriteFileTool};
use async_trait::async_trait;
use read_only_workers::{ReadOnlyWorkers, WorkerBudget, WorkerProfile};
use serde_json::json;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[derive(Clone, Copy)]
enum Mode {
    Answer,
    Read,
    Write,
    Recursive,
    Wait,
    Flood,
    Large,
    Unknown,
    Panic,
}
struct Model {
    mode: Mode,
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
}
impl Model {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }
}
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("scripted", model);
        spec.capabilities.tools = true;
        spec.context_tokens = Some(65536);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        if matches!(self.mode, Mode::Wait) {
            std::future::pending::<()>().await;
        }
        if matches!(self.mode, Mode::Panic) {
            panic!("scripted child owner failure");
        }
        let tool = request
            .messages
            .last()
            .is_some_and(|m| m.role == MessageRole::User)
            && matches!(self.mode, Mode::Read | Mode::Write | Mode::Recursive);
        let message = if tool {
            let name = match self.mode {
                Mode::Read => "read_file",
                Mode::Write => "write_file",
                _ => "worker",
            };
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "tool".into(),
                        name: name.into(),
                        arguments: if matches!(self.mode, Mode::Read) {
                            json!({"path":"code.txt"})
                        } else {
                            json!({"path":"code.txt","content":"must not write"})
                        },
                    },
                }],
            }
        } else {
            Message::text(
                MessageRole::Assistant,
                if matches!(self.mode, Mode::Large) {
                    "字".repeat(10000)
                } else if matches!(self.mode, Mode::Flood) {
                    "x".repeat(4000)
                } else {
                    "done".into()
                },
            )
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
            usage: if matches!(self.mode, Mode::Unknown) {
                ModelUsage::default()
            } else {
                ModelUsage {
                    input_tokens: Some(5),
                    output_tokens: Some(2),
                    total_tokens: Some(7),
                    ..Default::default()
                }
            },
        };
        if matches!(self.mode, Mode::Flood) {
            for _ in 0..4000 {
                sink.emit(ModelEvent::TextDelta { delta: "x".into() });
            }
        } else {
            emit_response(&response, sink.as_ref());
        }
        Ok(response)
    }
}
fn registry() -> ToolRegistry {
    let mut tools = ToolRegistry::default();
    tools.register(ReadFileTool).unwrap();
    tools
}
fn workers(
    root: &Path,
    model: Arc<Model>,
    repo: Arc<dyn SessionRepository>,
    budget: WorkerBudget,
    concurrent: usize,
) -> ReadOnlyWorkers {
    ReadOnlyWorkers::new(
        root,
        vec![WorkerProfile::new("review", root, model, "fixed-model", registry(), budget).unwrap()],
        repo,
        concurrent,
    )
    .unwrap()
}
async fn wait_started(model: &Model) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while model.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn workers_have_independent_canonical_history_lineage_traces_and_actual_usage() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("code.txt"), "source").unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let mut parent = repo.create(root.path()).unwrap();
    repo.append(
        &mut parent,
        Message::text(MessageRole::User, "private parent text not forwarded"),
    )
    .unwrap();
    let before = parent.entries().len();
    let model = Model::new(Mode::Read);
    let workers = workers(
        root.path(),
        model.clone(),
        repo.clone(),
        WorkerBudget::default(),
        2,
    );
    let a = workers
        .start("review", "review source".into(), Some(parent.metadata()))
        .unwrap();
    let b = workers
        .start("review", "second review".into(), Some(parent.metadata()))
        .unwrap();
    let a = workers.wait(&a).await.unwrap();
    let b = workers.wait(&b).await.unwrap();
    assert_eq!(a.status, "completed");
    assert_eq!(a.usage.as_ref().unwrap().total_tokens, Some(14));
    assert_eq!(a.tokens.as_ref().unwrap().used, 14);
    assert!(a.trace_complete);
    assert_ne!(a.session_id, b.session_id);
    assert_ne!(a.run_id, b.run_id);
    assert_eq!(
        repo.open(&parent.metadata().id).unwrap().entries().len(),
        before
    );
    let child = repo.open(a.session_id.as_deref().unwrap()).unwrap();
    assert!(child.entries().iter().any(|entry|matches!(&entry.data,SessionEntryData::Extension {namespace,data} if namespace=="example.worker_origin.v1" && data["parent"]["session_id"]==parent.metadata().id)));
    assert!(
        !serde_json::to_string(&model.requests.lock().unwrap().as_slice())
            .unwrap()
            .contains("private parent")
    );
    assert!(
        model
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.model == "fixed-model")
    );
    let page = workers.trace_page(&a.id, 0, 50).unwrap();
    assert!(page["frames"].as_array().unwrap().len() > 10);
    assert_eq!(page["capture_incomplete"], false);
    assert!(workers.trace_page(&a.id, usize::MAX, 50).is_err());
    assert!(workers.trace_page(&a.id, 0, 0).is_err());
    workers.shutdown().await;
}
#[tokio::test]
async fn profile_scope_and_command_shapes_cannot_select_privileged_routing() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let model = Model::new(Mode::Answer);
    let repo = Arc::new(MemorySessionRepository::new());
    let workers = workers(
        root.path(),
        model.clone(),
        repo.clone(),
        WorkerBudget::default(),
        1,
    );
    let foreign = repo.create(other.path()).unwrap();
    assert!(
        workers
            .start("review", "task".into(), Some(foreign.metadata()))
            .is_err()
    );
    assert!(
        workers
            .start("untrusted-model", "task".into(), None)
            .is_err()
    );
    assert!(workers.start("review", "x".repeat(8193), None).is_err());
    let mut tools = ToolRegistry::default();
    tools.register(WriteFileTool).unwrap();
    assert!(
        WorkerProfile::new(
            "bad",
            root.path(),
            model.clone(),
            "fixed",
            tools,
            WorkerBudget::default()
        )
        .is_err()
    );
    let mut ext = Extension::new("jobs", "1").unwrap();
    ext.command(Arc::new(workers.clone()), Duration::from_secs(1))
        .unwrap();
    let mut reg = ExtensionRegistry::new();
    reg.install(ext).unwrap();
    let extensions = reg.finish();
    for args in [
        json!({"action":"start","profile":"review","task":"task","model":"other"}),
        json!({"action":"start","profile":"review","task":"task","workspace":"/tmp"}),
        json!({"action":"start","profile":"review","task":"task","api_key":"fixture"}),
    ] {
        assert!(
            extensions
                .execute_command(
                    "worker",
                    CommandRequest {
                        context: CommandContext {
                            workspace: root.path(),
                            session: None
                        },
                        arguments: &args
                    },
                    &TurnExecutionContext::new()
                )
                .await
                .is_err()
        );
    }
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    workers.shutdown().await;
}
#[tokio::test]
async fn unavailable_writers_and_recursive_tools_cannot_escape_the_installed_registry() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    for mode in [Mode::Write, Mode::Recursive] {
        let workers = workers(
            root.path(),
            Model::new(mode),
            repo.clone(),
            WorkerBudget::default(),
            1,
        );
        let id = workers
            .start("review", "attempt privileged tool".into(), None)
            .unwrap();
        let report = workers.wait(&id).await.unwrap();
        assert_eq!(report.status, "completed");
        let session = repo.open(report.session_id.as_deref().unwrap()).unwrap();
        assert!(session.entries().iter().any(|entry|matches!(&entry.data,SessionEntryData::Message {message} if message.tool_result().is_some_and(|(_,result,_)|result["error"]["code"]=="unknown_tool"))));
        assert!(!root.path().join("code.txt").exists());
        workers.shutdown().await;
    }
}
#[tokio::test]
async fn per_worker_tool_token_output_duration_and_round_limits_use_core_outcomes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("code.txt"), "source").unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let cases = [
        (
            Mode::Read,
            WorkerBudget {
                tools: 0,
                ..Default::default()
            },
            run::BudgetResource::ToolCalls,
        ),
        (
            Mode::Answer,
            WorkerBudget {
                tokens: 128,
                ..Default::default()
            },
            run::BudgetResource::Tokens,
        ),
        (
            Mode::Large,
            WorkerBudget {
                output: 1024,
                ..Default::default()
            },
            run::BudgetResource::OutputBytes,
        ),
        (
            Mode::Wait,
            WorkerBudget {
                duration: Duration::from_millis(50),
                ..Default::default()
            },
            run::BudgetResource::Duration,
        ),
        (
            Mode::Read,
            WorkerBudget {
                rounds: 1,
                ..Default::default()
            },
            run::BudgetResource::Rounds,
        ),
    ];
    for (mode, budget, resource) in cases {
        let model = Model::new(mode);
        let workers = workers(root.path(), model.clone(), repo.clone(), budget, 1);
        let id = workers
            .start(
                "review",
                if resource == run::BudgetResource::Tokens {
                    "long task ".repeat(500)
                } else {
                    "review".into()
                },
                None,
            )
            .unwrap();
        let report = workers.wait(&id).await.unwrap();
        assert_eq!(
            report.status, "limited",
            "{} {:?}",
            report.status, report.stop
        );
        assert!(
            matches!(&report.stop,Some(RunStop::Budget(stop)) if stop.resource==resource),
            "{:?}",
            report.stop
        );
        if resource == run::BudgetResource::Tokens {
            assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        }
        workers.shutdown().await;
    }
}
#[tokio::test]
async fn cancellation_shutdown_and_concurrency_are_owned_and_do_not_restore_jobs_from_history() {
    let root = tempfile::tempdir().unwrap();
    let model = Model::new(Mode::Wait);
    let repo = Arc::new(MemorySessionRepository::new());
    let workers = workers(
        root.path(),
        model.clone(),
        repo.clone(),
        WorkerBudget::default(),
        1,
    );
    let id = workers.start("review", "wait".into(), None).unwrap();
    wait_started(&model).await;
    assert!(workers.start("review", "second".into(), None).is_err());
    assert!(workers.forget(&id).is_err());
    workers.cancel(&id).unwrap();
    let report = workers.wait(&id).await.unwrap();
    assert_eq!(report.status, "aborted");
    assert!(report.trace_complete);
    let child = repo.open(report.session_id.as_deref().unwrap()).unwrap();
    assert!(child.unfinished_turn().is_none());
    let second = workers.start("review", "wait again".into(), None).unwrap();
    workers.shutdown().await;
    let second = workers.wait(&second).await.unwrap();
    assert_eq!(second.status, "aborted");
    assert!(workers.start("review", "late".into(), None).is_err());
    workers.forget(&id).unwrap();
    assert!(workers.status(&id).is_err());
    let replacement = super_workers(root.path(), repo.clone());
    assert!(replacement.status(&second.id).is_err());
    replacement.shutdown().await;
}
fn super_workers(root: &Path, repo: Arc<dyn SessionRepository>) -> ReadOnlyWorkers {
    workers(
        root,
        Model::new(Mode::Answer),
        repo,
        WorkerBudget::default(),
        1,
    )
}
#[tokio::test]
async fn unknown_usage_and_incomplete_observation_are_not_invented_as_exact_or_complete() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    for mode in [Mode::Unknown, Mode::Flood, Mode::Panic] {
        let workers = workers(
            root.path(),
            Model::new(mode),
            repo.clone(),
            WorkerBudget::default(),
            1,
        );
        let id = workers.start("review", "answer".into(), None).unwrap();
        let report = workers.wait(&id).await.unwrap();
        match mode {
            Mode::Unknown => {
                assert!(report.tokens.as_ref().unwrap().estimated);
                assert_eq!(report.usage.as_ref().unwrap().total_tokens, None);
            }
            Mode::Flood => {
                assert_eq!(report.status, "completed");
                assert!(!report.trace_complete);
                assert_eq!(
                    workers.trace_page(&id, 0, 10).unwrap()["capture_incomplete"],
                    true
                );
            }
            Mode::Panic => {
                assert_eq!(report.status, "owner_failed");
                assert!(report.usage.is_none());
                assert!(!report.trace_complete);
            }
            _ => unreachable!(),
        };
        workers.shutdown().await;
    }
}
#[tokio::test]
async fn retained_jobs_and_lifetime_admissions_are_bounded_without_deleting_canonical_sessions() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let workers = workers(
        root.path(),
        Model::new(Mode::Answer),
        repo.clone(),
        WorkerBudget::default(),
        1,
    );
    let mut ids = Vec::new();
    let mut session = None;
    for _ in 0..8 {
        let id = workers.start("review", "answer".into(), None).unwrap();
        session = workers.wait(&id).await.unwrap().session_id.clone();
        ids.push(id);
    }
    assert!(
        workers
            .start("review", "over retention".into(), None)
            .is_err()
    );
    for id in &ids {
        workers.forget(id).unwrap();
    }
    for _ in 8..32 {
        let id = workers.start("review", "answer".into(), None).unwrap();
        workers.wait(&id).await.unwrap();
        workers.forget(&id).unwrap();
    }
    assert!(
        workers
            .start("review", "over lifetime".into(), None)
            .is_err()
    );
    assert!(repo.open(session.as_deref().unwrap()).is_ok());
    workers.shutdown().await;
}

#[test]
fn unpolled_owner_drop_is_reported_without_recreating_a_runtime_task() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let model = Model::new(Mode::Answer);
    let workers = workers(root.path(), model.clone(), repo, WorkerBudget::default(), 1);
    assert!(
        workers
            .start("review", "outside a runtime".into(), None)
            .is_err()
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let id = {
        let _entered = runtime.enter();
        workers
            .start("review", "never polled".into(), None)
            .unwrap()
    };
    drop(runtime);
    let report = workers.status(&id).unwrap();
    assert_eq!(report.status, "owner_failed");
    assert!(report.usage.is_none());
    assert!(!report.trace_complete);
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    let cleanup = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    cleanup.block_on(workers.shutdown());
}
#[tokio::test]
async fn dropping_the_host_cancels_owners_without_a_strong_manager_task_cycle() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let model = Model::new(Mode::Wait);
    let workers = workers(
        root.path(),
        model.clone(),
        repo.clone(),
        WorkerBudget::default(),
        1,
    );
    workers
        .start("review", "wait for host shutdown".into(), None)
        .unwrap();
    wait_started(&model).await;
    drop(workers);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let sessions = repo.list(root.path()).unwrap();
            if sessions.len() == 1 {
                let session = repo.open(&sessions[0].id).unwrap();
                if session.unfinished_turn().is_none()
                    && session
                        .entries()
                        .iter()
                        .any(|entry| matches!(entry.data, SessionEntryData::TurnEnd { .. }))
                {
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_waiters_observe_owner_completion_after_capacity_is_released() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let model = Model::new(Mode::Wait);
    let workers = workers(root.path(), model.clone(), repo, WorkerBudget::default(), 1);
    let id = workers.start("review", "wait".into(), None).unwrap();
    wait_started(&model).await;
    let a = workers.clone();
    let first = id.clone();
    let b = workers.clone();
    let second = id.clone();
    let a = tokio::spawn(async move { a.wait(&first).await.unwrap() });
    let b = tokio::spawn(async move { b.wait(&second).await.unwrap() });
    tokio::task::yield_now().await;
    workers.cancel(&id).unwrap();
    assert_eq!(a.await.unwrap().status, "aborted");
    assert_eq!(b.await.unwrap().status, "aborted");
    workers.start("review", "next".into(), None).unwrap();
    workers.shutdown().await;
}
