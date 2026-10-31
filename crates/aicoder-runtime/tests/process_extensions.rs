#![cfg(all(feature = "process-extension-fixtures", target_os = "linux"))]
use aicoder_core::{extensions::ExtensionRegistry, model::*, session::*, tools::*, *};
use aicoder_runtime::{process_extensions::*, session::JsonlSessionRepository};
use async_trait::async_trait;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

fn config(root: &Path, mode: &str) -> ProcessConfig {
    let mut config = ProcessConfig::new(
        env!("CARGO_BIN_EXE_aicoder-process-fixture"),
        root,
        "fixture",
    );
    config.arguments = vec![mode.into()];
    config.limits.request_timeout = Duration::from_secs(3);
    config
}
fn context(root: &Path) -> ToolContext {
    ToolContext::new(root, Duration::from_secs(5), 64 * 1024).unwrap()
}
fn tool(process: &ProcessExtension, name: &str) -> Arc<dyn ExecutableTool> {
    process
        .tools()
        .into_iter()
        .find(|t| t.definition().name == format!("fixture__{name}"))
        .unwrap()
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
fn dead(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |s| {
        s.rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
    })
}
struct Provider {
    tool: &'static str,
    requests: Mutex<Vec<ModelRequest>>,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let mut response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        if request.messages.last().unwrap().role == MessageRole::User {
            response.stop_reason = StopReason::ToolCalls;
            response.message.content = vec![ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "call".into(),
                    name: format!("fixture__{}", self.tool),
                    arguments: if self.tool == "write" {
                        json!({"path":"note.txt","content":"after"})
                    } else {
                        json!({"path":"note.txt"})
                    },
                },
            }];
        }
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Approval {
    allow: bool,
    conflict: bool,
    root: PathBuf,
}
#[async_trait]
impl ApprovalHandler for Approval {
    async fn approve(&self, invocation: &ToolInvocation) -> anyhow::Result<bool> {
        assert!(invocation.preview.as_ref().unwrap().body.contains("after"));
        assert_eq!(invocation.arguments["content"], "after");
        if self.conflict {
            fs::write(self.root.join("note.txt"), "changed after preparation")?;
        }
        Ok(self.allow)
    }
}
fn agent(
    root: &Path,
    process: &ProcessExtension,
    provider: Arc<Provider>,
    approval: impl ApprovalHandler + 'static,
) -> Agent {
    let mut registry = ExtensionRegistry::new();
    registry.install(process.extension().unwrap()).unwrap();
    Agent::new(
        TurnExecutor::builder_from_shared(provider)
            .workspace(root)
            .extensions(Arc::new(registry.finish()))
            .approval(approval)
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    )
}
#[tokio::test]
async fn prepared_process_tools_share_core_approval_conflicts_and_canonical_jsonl_without_tokens() {
    for (allow, conflict, expected) in [
        (true, false, "after"),
        (false, false, "before"),
        (true, true, "changed after preparation"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "before").unwrap();
        let process =
            ProcessExtension::connect(config(root.path(), "normal"), TurnExecutionContext::new())
                .await
                .unwrap();
        assert_eq!(
            tool(&process, "read").capability(),
            ToolCapability::Mutating
        );
        let provider = Arc::new(Provider {
            tool: "write",
            requests: Mutex::new(vec![]),
        });
        let agent = agent(
            root.path(),
            &process,
            provider.clone(),
            Approval {
                allow,
                conflict,
                root: root.path().into(),
            },
        );
        let repo = JsonlSessionRepository::new(state.path()).unwrap();
        let session = repo.create(root.path()).unwrap();
        agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "work",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("note.txt")).unwrap(),
            expected
        );
        until(|| process.diagnostics().completed_requests >= 3).await;
        let raw =
            fs::read_to_string(repo.open(&session.metadata().id).unwrap().path().unwrap()).unwrap();
        assert!(!raw.contains("opaque-plan"));
        {
            let requests = provider.requests.lock().unwrap();
            let result = requests
                .last()
                .unwrap()
                .messages
                .last()
                .unwrap()
                .tool_result()
                .unwrap()
                .1;
            if !allow {
                assert_eq!(result["error"]["code"], "approval_denied");
            } else if conflict {
                assert_eq!(result["error"]["code"], "process_tool_error");
            } else {
                assert_eq!(result["ok"], true);
            }
        }
        assert!(process.shutdown().await.reaped);
    }
}
#[tokio::test]
async fn plan_admission_disposal_and_connection_binding_are_explicit_and_bounded() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "before").unwrap();
    let mut cfg = config(root.path(), "normal");
    cfg.limits.max_plans = 1;
    let process = ProcessExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .unwrap();
    let tool = tool(&process, "write");
    let ctx = context(root.path());
    let plan = tool
        .prepare(&ctx, json!({"path":"note.txt","content":"after"}))
        .await
        .unwrap()
        .unwrap();
    let error = tool
        .prepare(&ctx, json!({"path":"note.txt","content":"second"}))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "process_plan_limit");
    assert_eq!(process.diagnostics().requests_sent, 2);
    drop(plan);
    until(|| process.diagnostics().completed_requests == 3).await;
    assert_eq!(
        fs::read_to_string(root.path().join("note.txt")).unwrap(),
        "before"
    );
    let old = tool
        .prepare(&ctx, json!({"path":"note.txt","content":"must not commit"}))
        .await
        .unwrap()
        .unwrap();
    assert!(process.shutdown().await.reaped);
    let replacement =
        ProcessExtension::connect(config(root.path(), "normal"), TurnExecutionContext::new())
            .await
            .unwrap();
    assert!(old.execute(&ctx).await.is_err());
    assert_eq!(replacement.diagnostics().requests_sent, 1);
    assert_eq!(
        fs::read_to_string(root.path().join("note.txt")).unwrap(),
        "before"
    );
    assert!(replacement.shutdown().await.reaped);
}
#[tokio::test]
async fn invalid_versions_catalogs_frames_and_stderr_fail_initialization_and_reap_the_child() {
    for mode in [
        "wrong_version",
        "duplicate_catalog",
        "bad_json",
        "schema_large",
        "stderr_flood",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(root.path(), mode);
        cfg.limits.max_stderr_bytes = 4096;
        let error = ProcessExtension::connect(cfg, TurnExecutionContext::new())
            .await
            .err()
            .unwrap();
        assert!(error.diagnostics.closed);
        assert!(error.diagnostics.reaped, "{mode}: {error:?}");
        assert!(!format!("{error:?}").contains("fixture-private-secret"));
    }
}
#[tokio::test]
async fn launch_identity_aliases_capabilities_and_workspace_scope_are_host_selected() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "data").unwrap();
    let mut cfg = config(root.path(), "normal");
    cfg.capabilities
        .insert("read".into(), ToolCapability::ReadOnly);
    cfg.aliases.insert("read".into(), "chosen_read".into());
    cfg.expected_server = Some(aicoder_core::extensions::process_protocol::ServerInfo {
        id: "fixture".into(),
        version: "1.0.0".into(),
    });
    let process = ProcessExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .unwrap();
    let read = process
        .tools()
        .into_iter()
        .find(|t| t.definition().name == "chosen_read")
        .unwrap();
    assert_eq!(read.capability(), ToolCapability::ReadOnly);
    let error = read
        .execute(&context(outside.path()), json!({"path":"note.txt"}))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "process_workspace");
    assert!(process.shutdown().await.reaped);
    let mut cfg = config(root.path(), "normal");
    cfg.aliases.insert("unknown".into(), "bad".into());
    assert!(
        ProcessExtension::connect(cfg, TurnExecutionContext::new())
            .await
            .is_err()
    );
    let mut cfg = config(root.path(), "normal");
    cfg.expected_server = Some(aicoder_core::extensions::process_protocol::ServerInfo {
        id: "fixture".into(),
        version: "other".into(),
    });
    assert!(
        ProcessExtension::connect(cfg, TurnExecutionContext::new())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn malformed_wrong_id_oversized_and_partial_replies_poison_without_automatic_retries() {
    for mode in ["wrong_id", "oversize", "exit_mid"] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "data").unwrap();
        let mut cfg = config(root.path(), mode);
        cfg.limits.max_frame_bytes = 16 * 1024;
        let process = ProcessExtension::connect(cfg, TurnExecutionContext::new())
            .await
            .unwrap();
        let read = tool(&process, "read");
        let ctx = context(root.path());
        let error = read
            .execute(&ctx, json!({"path":"note.txt"}))
            .await
            .err()
            .unwrap();
        assert_eq!(error.details.as_ref().unwrap()["may_have_executed"], true);
        assert_eq!(process.diagnostics().requests_sent, 2);
        assert!(
            read.execute(&ctx, json!({"path":"note.txt"}))
                .await
                .is_err()
        );
        assert_eq!(process.diagnostics().requests_sent, 2);
        let diagnostics = process.shutdown().await;
        assert!(diagnostics.reaped);
        if mode == "exit_mid" {
            assert_eq!(diagnostics.exit_code, Some(7));
        }
    }
}
#[tokio::test]
async fn complete_final_and_duplicate_replies_do_not_repeat_or_discard_completed_operations() {
    for mode in ["final_exit", "duplicate"] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "data").unwrap();
        let process =
            ProcessExtension::connect(config(root.path(), mode), TurnExecutionContext::new())
                .await
                .unwrap();
        let read = tool(&process, "read");
        let result = read
            .execute(&context(root.path()), json!({"path":"note.txt"}))
            .await
            .unwrap();
        if mode == "final_exit" {
            assert_eq!(result.output["large"].as_str().unwrap().len(), 128 * 1024);
        }
        until(|| process.diagnostics().closed).await;
        assert_eq!(process.diagnostics().completed_requests, 2);
        assert!(process.shutdown().await.reaped);
    }
}
#[tokio::test]
async fn active_core_cancellation_awaits_its_journal_and_stops_the_external_process() {
    for mode in ["slow", "commit_slow"] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "data").unwrap();
        let process =
            ProcessExtension::connect(config(root.path(), mode), TurnExecutionContext::new())
                .await
                .unwrap();
        let pid = process.diagnostics().pid.unwrap();
        let provider = Arc::new(Provider {
            tool: if mode == "commit_slow" {
                "write"
            } else {
                "read"
            },
            requests: Mutex::new(vec![]),
        });
        let agent = agent(root.path(), &process, provider, AllowAllApproval);
        let repo = Arc::new(MemorySessionRepository::new());
        let saved = repo.clone();
        let cancellation = TurnExecutionContext::new();
        let worker = cancellation.clone();
        let owner = tokio::spawn(async move {
            agent
                .run_with_session_outcome(
                    saved.as_ref(),
                    SessionSelection::New,
                    "read",
                    Arc::new(()),
                    worker,
                )
                .await
        });
        until(|| root.path().join("active").exists()).await;
        cancellation.cancel("stop");
        let result = owner.await.unwrap();
        assert!(matches!(result, AgentTurnOutcome::Aborted(_)));
        assert!(
            repo.open(&result.session().unwrap().id)
                .unwrap()
                .unfinished_turn()
                .is_none()
        );
        until(|| !Path::new(&format!("/proc/{pid}")).exists()).await;
        assert!(process.shutdown().await.reaped);
        assert_eq!(
            fs::read_to_string(root.path().join("note.txt")).unwrap(),
            "data"
        );
    }
}
#[tokio::test]
async fn direct_deadlines_drop_cleanup_and_same_group_descendants_are_not_runtime_owned() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "data").unwrap();
    let mut cfg = config(root.path(), "slow");
    cfg.limits.request_timeout = Duration::from_millis(200);
    let process = ProcessExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .unwrap();
    let read = tool(&process, "read");
    let ctx = context(root.path());
    let owner = tokio::spawn(async move { read.execute(&ctx, json!({"path":"note.txt"})).await });
    until(|| root.path().join("active").exists()).await;
    let error = owner.await.unwrap().err().unwrap();
    assert_eq!(error.details.unwrap()["may_have_executed"], true);
    assert!(process.shutdown().await.reaped);
    let process = ProcessExtension::connect(
        config(root.path(), "descendant"),
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    tool(&process, "read")
        .execute(&context(root.path()), json!({"path":"note.txt"}))
        .await
        .unwrap();
    let child = fs::read_to_string(root.path().join("descendant.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(process.shutdown().await.reaped);
    until(|| dead(child)).await;
}
#[tokio::test]
async fn cancelling_queued_work_does_not_cancel_active_work_and_busy_inputs_are_unsent() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "data").unwrap();
    let mut cfg = config(root.path(), "gate");
    cfg.limits.max_queued = 1;
    let process = ProcessExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .unwrap();
    let read = tool(&process, "read");
    let worker = read.clone();
    let ctx = context(root.path());
    let first = tokio::spawn(async move { worker.execute(&ctx, json!({"path":"note.txt"})).await });
    until(|| root.path().join("active").exists()).await;
    let worker = read.clone();
    let ctx = context(root.path());
    let queued =
        tokio::spawn(async move { worker.execute(&ctx, json!({"path":"note.txt"})).await });
    tokio::task::yield_now().await;
    let busy = read
        .execute(&context(root.path()), json!({"path":"note.txt"}))
        .await
        .err()
        .unwrap();
    assert_eq!(busy.code, "process_busy");
    assert_eq!(busy.details.unwrap()["may_have_executed"], false);
    queued.abort();
    let _ = queued.await;
    assert!(!process.diagnostics().closed);
    fs::write(root.path().join("release"), "release").unwrap();
    first.await.unwrap().unwrap();
    until(|| process.diagnostics().completed_requests == 2).await;
    assert_eq!(process.diagnostics().requests_sent, 2);
    assert!(process.shutdown().await.reaped);
}
#[tokio::test]
async fn launch_and_remote_failures_do_not_expose_arguments_environment_stderr_or_raw_error_text() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(root.path(), "remote_error");
    cfg.arguments.push("fixture-private-secret".into());
    cfg.environment
        .insert("PRIVATE_TOKEN".into(), "fixture-private-secret".into());
    assert!(!format!("{cfg:?}").contains("fixture-private-secret"));
    let process = ProcessExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .unwrap();
    let error = tool(&process, "read")
        .execute(&context(root.path()), json!({"path":"missing"}))
        .await
        .err()
        .unwrap();
    assert!(!format!("{error:?}").contains("fixture-private-secret"));
    assert_eq!(error.details.unwrap()["remote_code"], -32010);
    assert!(process.shutdown().await.reaped);
    let mut cfg = config(root.path(), "normal");
    cfg.program = "/aicoder-fixture-missing-program".into();
    let error = ProcessExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .err()
        .unwrap();
    assert_eq!(error.diagnostics.pid, None);
    assert_eq!(error.diagnostics.stop, Some(ProcessErrorKind::StartFailed));
}
#[tokio::test]
async fn cancelling_initialization_and_dropping_live_clients_reap_without_reconnecting() {
    let root = tempfile::tempdir().unwrap();
    let pidfile = root.path().join("initial.pid");
    let mut cfg = config(root.path(), "init_slow");
    cfg.arguments.push(pidfile.to_string_lossy().into_owned());
    let context = TurnExecutionContext::new();
    let worker = context.clone();
    let owner = tokio::spawn(async move { ProcessExtension::connect(cfg, worker).await });
    until(|| pidfile.exists()).await;
    context.cancel("stop");
    let error = owner.await.unwrap().err().unwrap();
    assert_eq!(error.kind, ProcessErrorKind::Cancelled);
    assert!(error.diagnostics.reaped);
    let process =
        ProcessExtension::connect(config(root.path(), "normal"), TurnExecutionContext::new())
            .await
            .unwrap();
    let pid = process.diagnostics().pid.unwrap();
    drop(process);
    until(|| !Path::new(&format!("/proc/{pid}")).exists()).await;
}
#[test]
fn dropping_a_runtime_still_signals_and_reaps_an_active_protocol_owner() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "data").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let process = runtime
        .block_on(ProcessExtension::connect(
            config(root.path(), "slow"),
            TurnExecutionContext::new(),
        ))
        .unwrap();
    let pid = process.diagnostics().pid.unwrap();
    let read = tool(&process, "read");
    let ctx = context(root.path());
    runtime.spawn(async move {
        let _ = read.execute(&ctx, json!({"path":"note.txt"})).await;
    });
    runtime.block_on(until(|| root.path().join("active").exists()));
    drop(runtime);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while (Path::new(&format!("/proc/{pid}")).exists() || !process.diagnostics().worker_done)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert!(process.diagnostics().reaped);
}

#[tokio::test]
async fn invalid_launch_settings_and_oversize_requests_are_rejected_before_dispatch() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "data").unwrap();
    let mut cfg = config(root.path(), "normal");
    cfg.arguments.push("bad\0argument".into());
    let error = ProcessExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, ProcessErrorKind::InvalidConfiguration);
    assert_eq!(error.diagnostics.pid, None);
    let process =
        ProcessExtension::connect(config(root.path(), "normal"), TurnExecutionContext::new())
            .await
            .unwrap();
    let read = tool(&process, "read");
    let error = read
        .execute(&context(root.path()), json!({"path":"x".repeat(256*1024)}))
        .await
        .err()
        .unwrap();
    assert_eq!(error.details.unwrap()["may_have_executed"], false);
    assert_eq!(process.diagnostics().requests_sent, 1);
    read.execute(&context(root.path()), json!({"path":"note.txt"}))
        .await
        .unwrap();
    assert_eq!(process.diagnostics().requests_sent, 2);
    assert!(process.shutdown().await.reaped);
}
