#![cfg(feature = "auxiliary-models")]
use aicoder_core::{auxiliary::*, model::*, session::*, tools::*, *};
use aicoder_runtime::{resources::ResourceSettings, *};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
struct Model {
    calls: Mutex<Vec<ModelRequest>>,
    program: Option<String>,
}
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.tools = true;
        spec.context_tokens = Some(65536);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        _: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let first = !request.messages.iter().any(|m| m.role == MessageRole::Tool);
        self.calls.lock().unwrap().push(request.clone());
        let (name, args) = if let Some(code) = &self.program {
            ("codemode", json!({"code":code}))
        } else {
            (
                "model_classify",
                json!({"model":{"provider":"judge","id":"native"},"context":{"state":{"text":"sample"},"questions":{"ok":{"type":"bool","instructions":"Is it valid?","criteria":{"true":"valid","false":"invalid"}},"label":{"type":"choice","instructions":"Pick","criteria":{"yes":"yes","no":"no"}},"rank":{"type":"score","instructions":"Rank","criteria":["low","high"]}}}}),
            )
        };
        Ok(ModelResponse {
            id: None,
            model: request.model,
            message: if first {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "task".into(),
                            name: name.into(),
                            arguments: args,
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::Assistant, "done")
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
        })
    }
}
async fn request(listener: &TcpListener) -> (tokio::net::TcpStream, Value) {
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    let end = loop {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(at) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            break at + 4;
        }
        assert!(bytes.len() < 16384);
    };
    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
    assert!(headers.starts_with("POST /v1/systemone "));
    assert!(
        headers
            .to_lowercase()
            .contains("authorization: bearer fixture-judge")
    );
    let size = headers
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    assert!(size < 2 * 1024 * 1024);
    while bytes.len() < end + size {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
    }
    (
        socket,
        serde_json::from_slice(&bytes[end..end + size]).unwrap(),
    )
}
async fn respond(mut socket: tokio::net::TcpStream, body: Value) {
    let body = body.to_string();
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
}
fn runtime(
    root: &std::path::Path,
    state: &std::path::Path,
    address: std::net::SocketAddr,
    program: bool,
) -> Runtime {
    let file = state.join("settings.json");
    fs::write(&file,json!({"version":1,"profiles":{"native":{"provider_id":"judge","base_url":format!("http://{address}/v1"),"api_key_env":"JUDGE_KEY","proxy":"disabled","classifiers":[{"provider":"judge","id":"native","name":"Native fixture","context_tokens":65536}]}},"settings":{"code_mode":{"enabled":program,"only":program,"model_profiles":["native"]}}}).to_string()).unwrap();
    Runtime::load_with_environment(
        root,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Value(state.join("sessions")),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: true,
            user_file: Some(file),
            project_file: None,
        },
        Arc::new(BTreeMap::from([(
            "JUDGE_KEY".into(),
            "fixture-judge".into(),
        )])),
    )
    .unwrap()
}
#[tokio::test]
async fn native_classification_charges_usage_and_persists_task_records_before_delivery() {
    for valid in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, body) = request(&listener).await;
            assert_eq!(body["model"], "native");
            assert_eq!(body["questions"]["ok"]["type"], "noul");
            respond(socket,json!({"answers":{"ok":{"type":"noul","noul":if valid{0.8}else{2.0}},"label":{"type":"choice","choice":"yes","probabilities":{"yes":0.8,"no":0.2},"confidence":0.8},"rank":{"type":"score","score":0.75,"confidence":0.9}},"usage":{"input_tokens":11,"output_tokens":0}})).await;
        });
        let runtime = runtime(root.path(), state.path(), address, false);
        let provider = Arc::new(Model {
            calls: Mutex::new(vec![]),
            program: None,
        });
        let agent = Agent::new(
            runtime
                .executor_builder(provider.clone())
                .unwrap()
                .approval(AllowAllApproval)
                .build()
                .unwrap(),
            AgentConfig::new("fixture"),
        );
        let repo = runtime.sessions().unwrap();
        let session = repo.create(root.path()).unwrap();
        let turn = agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "classify",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
        server.await.unwrap();
        assert_eq!(turn.execution_result.usage.total_tokens, Some(25));
        assert_eq!(
            turn.execution_result
                .primary_usage
                .as_ref()
                .unwrap()
                .total_tokens,
            Some(14)
        );
        assert_eq!(turn.execution_result.auxiliary.len(), 1);
        let task = &turn.execution_result.auxiliary[0];
        assert_eq!(task.usage.total_tokens, Some(11));
        assert_eq!(task.status, if valid { "completed" } else { "error" });
        assert_eq!(task.delivery_complete, valid);
        assert!(!task.tokens.estimated);
        let result = turn
            .execution_result
            .messages
            .iter()
            .find_map(Message::tool_result)
            .unwrap();
        assert_eq!(result.2, !valid);
        if valid {
            assert_eq!(result.1["output"]["answers"]["ok"]["probability"], 0.8);
        } else {
            assert_eq!(result.1["error"]["details"]["usage"]["total_tokens"], 11);
        }
        let saved = repo.open(&session.metadata().id).unwrap();
        let records = saved
            .entries()
            .iter()
            .filter_map(|entry| match &entry.data {
                SessionEntryData::Extension { namespace, data }
                    if namespace == AUXILIARY_NAMESPACE =>
                {
                    Some(data.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["kind"], "started");
        assert_eq!(records[1]["kind"], "finished");
        assert_eq!(provider.calls.lock().unwrap().len(), 2);
    }
}
#[cfg(feature = "code-mode")]
#[tokio::test]
async fn code_mode_model_catalog_and_typed_helper_share_the_native_meter() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = request(&listener).await;
        respond(socket,json!({"answers":{"ok":{"type":"noul","noul":0.9}},"usage":{"input_tokens":11,"output_tokens":0}})).await;
    });
    let runtime = runtime(root.path(), state.path(), address, true);
    let code = "const found=await models.getAvailableOfType('classifier'); const m=await models.getModelOfType('classifier','judge','native'); const result=await models.classify(m,{state:{text:'sample'},questions:{ok:{type:'bool',instructions:'Is it valid?',criteria:{true:'valid',false:'invalid'}}}});text([found.length,result.stopReason,result.answers.ok.probability]);";
    let provider = Arc::new(Model {
        calls: Mutex::new(vec![]),
        program: Some(code.into()),
    });
    let agent = Agent::new(
        runtime
            .executor_builder(provider.clone())
            .unwrap()
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let turn = agent.run("classify", Arc::new(())).await.unwrap();
    server.await.unwrap();
    assert_eq!(turn.execution_result.usage.total_tokens, Some(25));
    let result = turn
        .execution_result
        .messages
        .iter()
        .find_map(Message::tool_result)
        .unwrap();
    assert!(!result.2, "{}", result.1);
    assert_eq!(
        result.1["output"]["program"]["items"],
        json!([[1, "stop", 0.9]])
    );
    assert_eq!(turn.execution_result.tool_calls, 4);
    assert!(
        provider
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.tools.len() == 1 && request.tools[0].name == "codemode")
    );
}

#[cfg(feature = "code-mode")]
#[tokio::test]
async fn concurrent_native_tasks_reserve_parent_tokens_atomically_and_bound_inflight_requests() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let maximum = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let current = active.clone();
    let observed = maximum.clone();
    let server = tokio::spawn(async move {
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let (socket, _) = request(&listener).await;
            let now = current.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            observed.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
            let current = current.clone();
            tasks.push(tokio::spawn(async move{tokio::time::sleep(Duration::from_millis(25)).await;respond(socket,json!({"answers":{"ok":{"type":"noul","noul":0.8}},"usage":{"input_tokens":11,"output_tokens":0}})).await;current.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);}));
        }
        for task in tasks {
            task.await.unwrap();
        }
    });
    let runtime = runtime(root.path(), state.path(), address, true);
    let code = "const m=await models.getModelOfType('classifier','judge','native');const r=await Promise.all(Array.from({length:8},()=>models.classify(m,{state:{x:1},questions:{ok:{type:'bool',instructions:'valid?',criteria:{true:'yes',false:'no'}}}})));text(r.map(x=>x.stopReason));";
    let provider = Arc::new(Model {
        calls: Mutex::new(vec![]),
        program: Some(code.into()),
    });
    let agent = Agent::new(
        runtime
            .executor_builder(provider)
            .unwrap()
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let turn = agent.run("classify", Arc::new(())).await.unwrap();
    server.await.unwrap();
    assert_eq!(maximum.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert_eq!(turn.execution_result.auxiliary.len(), 8);
    assert_eq!(turn.execution_result.usage.total_tokens, Some(102));
    assert_eq!(turn.execution_result.tokens.used, 102);
}
#[tokio::test]
async fn zero_task_budget_fails_without_provider_io() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let runtime = runtime(
        root.path(),
        state.path(),
        listener.local_addr().unwrap(),
        false,
    );
    let provider = Arc::new(Model {
        calls: Mutex::new(vec![]),
        program: None,
    });
    let agent = Agent::new(
        runtime
            .executor_builder(provider.clone())
            .unwrap()
            .approval(AllowAllApproval)
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_auxiliary_calls: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let result = agent
        .run_outcome("classify", Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(result, AgentTurnOutcome::Limited(_)));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    assert_eq!(provider.calls.lock().unwrap().len(), 1);
    assert!(result.progress().auxiliary.is_empty());
}
#[tokio::test]
async fn cancellation_retains_unknown_usage_and_an_uncertain_started_request_without_retries() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let notify = started.clone();
    let server = tokio::spawn(async move {
        let (socket, _) = request(&listener).await;
        notify.notify_one();
        let _socket = socket;
        std::future::pending::<()>().await;
    });
    let runtime = runtime(root.path(), state.path(), address, false);
    let provider = Arc::new(Model {
        calls: Mutex::new(vec![]),
        program: None,
    });
    let agent = Agent::new(
        runtime
            .executor_builder(provider.clone())
            .unwrap()
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let context = TurnExecutionContext::new();
    let worker = context.clone();
    let task =
        tokio::spawn(async move { agent.run_outcome("classify", Arc::new(()), worker).await });
    started.notified().await;
    context.cancel("cancel native task");
    let outcome = task.await.unwrap();
    assert!(!matches!(outcome, AgentTurnOutcome::Completed(_)));
    let progress = outcome.progress();
    assert_eq!(progress.auxiliary.len(), 1);
    assert_eq!(progress.auxiliary[0].status, "interrupted");
    assert!(!progress.auxiliary[0].reported_usage);
    assert!(progress.auxiliary[0].tokens.estimated);
    assert!(progress.auxiliary[0].tokens.used > 0);
    assert!(progress.usage.total_tokens.is_none());
    assert_eq!(
        progress.primary_usage.as_ref().unwrap().total_tokens,
        Some(7)
    );
    assert_eq!(provider.calls.lock().unwrap().len(), 1);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn unsupported_task_journals_stop_before_http_with_persistence_failure() {
    struct Journal;
    #[async_trait]
    impl RunJournal for Journal {
        async fn checkpoint(&self, _: usize, _: &Message) -> anyhow::Result<()> {
            Ok(())
        }
        async fn finish(&self, _: &RunOutcome) -> anyhow::Result<()> {
            Ok(())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let runtime = runtime(
        root.path(),
        state.path(),
        listener.local_addr().unwrap(),
        false,
    );
    let provider = Arc::new(Model {
        calls: Mutex::new(vec![]),
        program: None,
    });
    let executor = runtime
        .executor_builder(provider)
        .unwrap()
        .approval(AllowAllApproval)
        .build()
        .unwrap();
    let outcome = executor
        .run_with_journal(
            ModelRequest::new(
                "fixture",
                vec![Message::text(MessageRole::User, "classify")],
            ),
            Arc::new(()),
            TurnExecutionContext::new(),
            &Journal,
        )
        .await;
    assert!(
        matches!(outcome,RunOutcome::Interrupted(ref interrupted) if matches!(&interrupted.stop,RunStop::Failed(failure) if failure.stage==RunStage::Persistence))
    );
    assert!(outcome.progress().auxiliary.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[test]
fn task_routes_stay_out_of_snapshots_and_project_settings_cannot_create_credential_routes() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let runtime = runtime(
        root.path(),
        state.path(),
        "127.0.0.1:1".parse().unwrap(),
        false,
    );
    let snapshot = serde_json::to_string(runtime.config()).unwrap();
    assert!(!snapshot.contains("fixture-judge"));
    assert!(!snapshot.contains("classifier_routes"));
    assert!(!snapshot.contains("/systemone"));
    let project = root.path().join("project.json");
    fs::write(&project,json!({"version":1,"profiles":{"injected":{"base_url":"https://must-not-contact.invalid","api_key_env":"ATTACK_KEY"}}}).to_string()).unwrap();
    let result = Runtime::load_with_environment(
        root.path(),
        RuntimeOverrides::default(),
        ConfigDiscovery {
            enabled: true,
            user_file: Some(state.path().join("settings.json")),
            project_file: Some(project),
        },
        Arc::new(BTreeMap::new()),
    );
    assert!(result.is_err());
}
