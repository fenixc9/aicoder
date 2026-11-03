#![cfg(all(feature = "mcp-fixtures", target_os = "linux"))]
use aicoder_core::{
    artifacts::{ArtifactStore, MemoryArtifactStore},
    extensions::ExtensionRegistry,
    model::*,
    session::*,
    tools::*,
    *,
};
use aicoder_runtime::{mcp::*, process_extensions::ProcessConfig, session::JsonlSessionRepository};
use async_trait::async_trait;
use serde_json::json;
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
fn config(root: &Path, mode: &str) -> McpConfig {
    let mut process =
        ProcessConfig::new(env!("CARGO_BIN_EXE_aicoder-mcp-fixture"), root, "fixture");
    process.arguments = vec![mode.into()];
    let mut config = McpConfig::new(process);
    config.limits.probe_timeout = Duration::from_millis(100);
    config
}
fn context(root: &Path) -> ToolContext {
    ToolContext::new(root, Duration::from_secs(3), 65536).unwrap()
}
fn read(extension: &McpExtension) -> Arc<dyn ExecutableTool> {
    extension
        .tools()
        .into_iter()
        .find(|t| t.definition().name == "fixture__read")
        .unwrap()
}
async fn until(mut test: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn modern_and_legacy_discovery_respect_wire_era_pagination_and_explicit_shutdown() {
    for mode in [
        "modern",
        "legacy",
        "legacy_params",
        "legacy_silent",
        "legacy_late",
        "legacy_old",
        "legacy_ping",
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "before").unwrap();
        let extension =
            McpExtension::connect(config(root.path(), mode), TurnExecutionContext::new())
                .await
                .unwrap();
        assert_eq!(extension.catalog().len(), 2);
        assert_eq!(read(&extension).capability(), ToolCapability::Mutating);
        assert_eq!(extension.server().name, "fixture/mcp");
        if mode == "modern" {
            assert_eq!(extension.protocol_version(), MODERN_VERSION);
        } else {
            assert!(LEGACY_VERSIONS.contains(&extension.protocol_version()));
        }
        read(&extension)
            .execute(&context(root.path()), json!({"path":"note.txt"}))
            .await
            .unwrap();
        if mode == "legacy_ping" {
            assert_eq!(extension.wire_diagnostics().legacy_pings, 1);
        }
        let diagnostics = extension.shutdown().await;
        assert!(diagnostics.reaped, "{mode}: {diagnostics:?}");
        assert!(root.path().join("server_eof").exists());
    }
}
struct Provider(Mutex<Vec<ModelRequest>>);
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
        self.0.lock().unwrap().push(request.clone());
        let mut result = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        if request.messages.last().unwrap().role == MessageRole::User {
            result.stop_reason = StopReason::ToolCalls;
            result.message.content = vec![ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "mcp-write".into(),
                    name: "fixture__write".into(),
                    arguments: json!({"path":"note.txt","content":"after"}),
                },
            }];
        }
        emit_response(&result, sink.as_ref());
        Ok(result)
    }
}
#[tokio::test]
async fn core_approval_and_canonical_history_apply_without_server_instructions_or_private_meta() {
    for allow in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "before").unwrap();
        let extension =
            McpExtension::connect(config(root.path(), "modern"), TurnExecutionContext::new())
                .await
                .unwrap();
        let mut registry = ExtensionRegistry::new();
        registry.install(extension.extension().unwrap()).unwrap();
        let provider = Arc::new(Provider(Mutex::new(vec![])));
        let builder = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(Arc::new(registry.finish()));
        let executor = if allow {
            builder.approval(AllowAllApproval).build().unwrap()
        } else {
            builder.build().unwrap()
        };
        let agent = Agent::new(executor, AgentConfig::new("m"));
        let repo = JsonlSessionRepository::new(state.path()).unwrap();
        let session = repo.create(root.path()).unwrap();
        agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "write",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("note.txt")).unwrap(),
            if allow { "after" } else { "before" }
        );
        let data =
            fs::read_to_string(repo.open(&session.metadata().id).unwrap().path().unwrap()).unwrap();
        assert!(!data.contains("DO_NOT_PERSIST_PRIVATE"));
        assert!(!data.contains("DO_NOT_MAKE_THIS"));
        {
            let requests = provider.0.lock().unwrap();
            assert!(requests.iter().all(|r| {
                !serde_json::to_string(&r.messages)
                    .unwrap()
                    .contains("DO_NOT_MAKE_THIS")
            }));
        }
        assert_eq!(
            extension.diagnostics().requests_sent,
            if allow { 4 } else { 3 }
        );
        assert!(extension.shutdown().await.reaped);
    }
}
#[tokio::test]
async fn text_structured_errors_and_all_media_preserve_completed_calls_without_base64_text() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "before").unwrap();
    let extension =
        McpExtension::connect(config(root.path(), "media"), TurnExecutionContext::new())
            .await
            .unwrap();
    let store = Arc::new(MemoryArtifactStore::default());
    let ctx = context(root.path()).with_artifact_store(store.clone());
    let result = read(&extension)
        .execute(&ctx, json!({"path":"note.txt"}))
        .await
        .unwrap();
    assert_eq!(result.artifacts.len(), 3);
    assert_eq!(result.output["delivery_complete"], true);
    assert!(result.output["structuredContent"].is_object());
    assert!(!result.output.to_string().contains("AQID"));
    assert!(!result.output.to_string().contains("DO_NOT_PERSIST"));
    for reference in &result.artifacts {
        assert!(reference.complete);
        assert_eq!(
            store.read(&reference.id, 0, 3).await.unwrap().bytes.len(),
            3
        );
    }
    let result = read(&extension)
        .execute(&context(root.path()), json!({"path":"note.txt"}))
        .await
        .unwrap();
    assert!(result.truncated);
    assert_eq!(result.output["operation_completed"], true);
    assert_eq!(result.output["delivery_complete"], false);
    assert_eq!(extension.diagnostics().requests_sent, 5);
    assert!(extension.shutdown().await.reaped);
    let errors = McpExtension::connect(
        config(root.path(), "tool_error"),
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    let error = read(&errors)
        .execute(&context(root.path()), json!({"path":"note.txt"}))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "mcp_tool_error");
    assert_eq!(error.details.unwrap()["operation_completed"], true);
    errors.shutdown().await;
}
#[tokio::test]
async fn catalog_notifications_ttl_and_refresh_cannot_rewrite_a_live_registration() {
    for mode in ["ttl_zero", "legacy_list_changed"] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "before").unwrap();
        let extension =
            McpExtension::connect(config(root.path(), mode), TurnExecutionContext::new())
                .await
                .unwrap();
        let tool = read(&extension);
        let ctx = context(root.path());
        tool.execute(&ctx, json!({"path":"note.txt"}))
            .await
            .unwrap();
        fs::write(root.path().join("catalog_changed"), "changed").unwrap();
        if mode == "legacy_list_changed" {
            until(|| extension.wire_diagnostics().catalog_epoch > 0).await;
        }
        let error = tool
            .execute(&ctx, json!({"path":"note.txt"}))
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "mcp_catalog_stale");
        let fresh = extension
            .refresh(TurnExecutionContext::new())
            .await
            .unwrap();
        assert_ne!(fresh.catalog(), extension.catalog());
        read(&fresh)
            .execute(&ctx, json!({"path":"note.txt"}))
            .await
            .unwrap();
        fs::remove_file(root.path().join("catalog_changed")).unwrap();
        assert!(
            tool.execute(&ctx, json!({"path":"note.txt"}))
                .await
                .is_err()
        );
        fresh.shutdown().await;
    }
}
#[tokio::test]
async fn unsupported_modern_versions_pagination_cycles_and_unoffered_interactions_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "before").unwrap();
    let error = McpExtension::connect(
        config(root.path(), "unsupported_modern"),
        TurnExecutionContext::new(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, McpErrorKind::UnsupportedVersion);
    assert_eq!(error.supported_versions, vec!["2099-01-01"]);
    let error = McpExtension::connect(
        config(root.path(), "cursor_loop"),
        TurnExecutionContext::new(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, McpErrorKind::InvalidCatalog);
    let extension = McpExtension::connect(
        config(root.path(), "input_required"),
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    let error = read(&extension)
        .execute(&context(root.path()), json!({"path":"note.txt"}))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "mcp_interaction_unavailable");
    assert!(!format!("{error:?}").contains("PRIVATE_CONTINUATION"));
    extension.shutdown().await;
    let extension = McpExtension::connect(
        config(root.path(), "legacy_request"),
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    read(&extension)
        .execute(&context(root.path()), json!({"path":"note.txt"}))
        .await
        .unwrap();
    until(|| extension.wire_diagnostics().rejected_server_requests == 1).await;
    extension.shutdown().await;
    assert_eq!(extension.wire_diagnostics().control_replies_written, 1);
}
#[tokio::test]
async fn cancellation_sends_scoped_notification_then_reaps_and_does_not_replay() {
    for mode in ["slow", "legacy_slow"] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "before").unwrap();
        let extension =
            McpExtension::connect(config(root.path(), mode), TurnExecutionContext::new())
                .await
                .unwrap();
        let tool = read(&extension);
        let ctx = context(root.path());
        let owner =
            tokio::spawn(async move { tool.execute(&ctx, json!({"path":"note.txt"})).await });
        until(|| root.path().join("active").exists()).await;
        owner.abort();
        let _ = owner.await;
        let diagnostics = extension.shutdown().await;
        assert!(diagnostics.reaped);
        assert_eq!(extension.wire_diagnostics().cancellation_written, 1);
        assert!(root.path().join("cancelled").exists());
        assert_eq!(
            fs::read_to_string(root.path().join("note.txt")).unwrap(),
            "before"
        );
    }
}
#[tokio::test]
async fn long_remote_names_get_stable_bounded_aliases_and_host_overrides_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "before").unwrap();
    let mut cfg = config(root.path(), "long_names");
    let remote = "x".repeat(100);
    cfg.capabilities
        .insert(remote.clone(), ToolCapability::ReadOnly);
    let extension = McpExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .unwrap();
    let tool = extension.tools().remove(0);
    assert!(tool.definition().name.len() <= 64);
    assert_eq!(tool.capability(), ToolCapability::ReadOnly);
    let result = tool
        .execute(&context(root.path()), json!({"path":"note.txt"}))
        .await
        .unwrap();
    assert_eq!(result.output["structuredContent"]["remote_name"], remote);
    extension.shutdown().await;
}

#[tokio::test]
async fn catalog_and_notification_budgets_bound_untrusted_control_traffic() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "before").unwrap();
    let mut cfg = config(root.path(), "modern");
    cfg.limits.max_pages = 1;
    assert_eq!(
        McpExtension::connect(cfg, TurnExecutionContext::new())
            .await
            .err()
            .unwrap()
            .kind,
        McpErrorKind::CatalogLimit
    );
    let mut cfg = config(root.path(), "flood");
    cfg.limits.max_aux_messages_per_second = 8;
    let extension = McpExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .unwrap();
    assert!(
        read(&extension)
            .execute(&context(root.path()), json!({"path":"note.txt"}))
            .await
            .is_err()
    );
    assert!(extension.wire_diagnostics().notifications_received <= 8);
    assert!(extension.shutdown().await.reaped);
}

#[tokio::test]
async fn explicit_protocol_profiles_identity_and_launch_authority_are_not_peer_controlled() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(root.path(), "legacy_silent");
    cfg.protocol = McpProtocol::Legacy {
        version: "2025-11-25".into(),
    };
    let extension = McpExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(extension.diagnostics().requests_sent, 3);
    extension.shutdown().await;
    let mut cfg = config(root.path(), "legacy");
    cfg.protocol = McpProtocol::Modern;
    assert_eq!(
        McpExtension::connect(cfg, TurnExecutionContext::new())
            .await
            .err()
            .unwrap()
            .kind,
        McpErrorKind::UnsupportedVersion
    );
    let mut cfg = config(root.path(), "modern");
    cfg.expected_server = Some(McpServerInfo {
        name: "another server".into(),
        version: "1".into(),
    });
    assert_eq!(
        McpExtension::connect(cfg, TurnExecutionContext::new())
            .await
            .err()
            .unwrap()
            .kind,
        McpErrorKind::IdentityMismatch
    );
    let mut cfg = config(root.path(), "modern");
    cfg.process
        .capabilities
        .insert("read".into(), ToolCapability::ReadOnly);
    let error = McpExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .err()
        .unwrap();
    assert_eq!(error.process.unwrap().diagnostics.pid, None);
}

#[tokio::test]
async fn core_cancellation_closes_history_and_initialization_never_sends_cancelled_for_initialize()
{
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "before").unwrap();
    let extension = McpExtension::connect(config(root.path(), "slow"), TurnExecutionContext::new())
        .await
        .unwrap();
    let mut registry = ExtensionRegistry::new();
    registry.install(extension.extension().unwrap()).unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(Provider(Mutex::new(vec![])))
            .workspace(root.path())
            .extensions(Arc::new(registry.finish()))
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let repo = Arc::new(JsonlSessionRepository::new(state.path()).unwrap());
    let saved = repo.clone();
    let context = TurnExecutionContext::new();
    let worker = context.clone();
    let owner = tokio::spawn(async move {
        agent
            .run_with_session_outcome(
                saved.as_ref(),
                SessionSelection::New,
                "work",
                Arc::new(()),
                worker,
            )
            .await
    });
    until(|| root.path().join("active").exists()).await;
    context.cancel("stop");
    let result = owner.await.unwrap();
    assert!(matches!(result, AgentTurnOutcome::Aborted(_)));
    assert!(
        repo.open(&result.session().unwrap().id)
            .unwrap()
            .unfinished_turn()
            .is_none()
    );
    assert!(extension.shutdown().await.reaped);
    fs::remove_file(root.path().join("cancelled")).unwrap();
    let mut cfg = config(root.path(), "legacy_init_slow");
    cfg.protocol = McpProtocol::Legacy {
        version: "2025-11-25".into(),
    };
    let pidfile = root.path().join("pid");
    cfg.process
        .arguments
        .push(pidfile.to_string_lossy().into_owned());
    let context = TurnExecutionContext::new();
    let worker = context.clone();
    let owner = tokio::spawn(async move { McpExtension::connect(cfg, worker).await });
    until(|| root.path().join("initializing").exists()).await;
    context.cancel("stop");
    assert_eq!(
        owner.await.unwrap().err().unwrap().kind,
        McpErrorKind::Cancelled
    );
    assert!(!root.path().join("cancelled").exists());
    let pid = fs::read_to_string(pidfile).unwrap();
    until(|| !Path::new(&format!("/proc/{pid}")).exists()).await;
}

#[tokio::test]
async fn media_admission_and_invalid_content_do_not_repeat_completed_tool_operations() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "before").unwrap();
    let mut cfg = config(root.path(), "media");
    cfg.limits.max_media_bytes = 3;
    let extension = McpExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .unwrap();
    let store = Arc::new(MemoryArtifactStore::default());
    let ctx = context(root.path()).with_artifact_store(store);
    let result = read(&extension)
        .execute(&ctx, json!({"path":"note.txt"}))
        .await
        .unwrap();
    assert_eq!(result.artifacts.len(), 1);
    assert_eq!(result.output["delivery_complete"], false);
    assert_eq!(extension.diagnostics().requests_sent, 4);
    extension.shutdown().await;
    let mut cfg = config(root.path(), "media");
    cfg.limits.max_content_blocks = 1;
    let extension = McpExtension::connect(cfg, TurnExecutionContext::new())
        .await
        .unwrap();
    let error = read(&extension)
        .execute(&context(root.path()), json!({"path":"note.txt"}))
        .await
        .err()
        .unwrap();
    assert_eq!(error.details.unwrap()["operation_completed"], true);
    assert_eq!(extension.diagnostics().requests_sent, 4);
    extension.shutdown().await;
}

#[tokio::test]
async fn tool_budget_and_versioned_core_trace_cover_mcp_without_dispatching_a_forbidden_call() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "before").unwrap();
    let extension =
        McpExtension::connect(config(root.path(), "modern"), TurnExecutionContext::new())
            .await
            .unwrap();
    let mut registry = ExtensionRegistry::new();
    registry.install(extension.extension().unwrap()).unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(Provider(Mutex::new(vec![])))
            .workspace(root.path())
            .extensions(Arc::new(registry.finish()))
            .approval(AllowAllApproval)
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_tool_calls: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let frames = Arc::new(Mutex::new(vec![]));
    let target = frames.clone();
    let observer =
        Arc::new(move |event: &AgentRawEventEnvelope| target.lock().unwrap().push(event.clone()));
    let result = agent
        .run_outcome("work", observer, TurnExecutionContext::new())
        .await;
    assert!(matches!(result, AgentTurnOutcome::Limited(_)));
    assert_eq!(extension.diagnostics().requests_sent, 3);
    assert_eq!(
        fs::read_to_string(root.path().join("note.txt")).unwrap(),
        "before"
    );
    let mut validator = events::EventTraceValidator::new();
    for frame in frames.lock().unwrap().iter() {
        validator.observe(frame).unwrap();
    }
    validator.finish().unwrap();
    assert!(extension.shutdown().await.reaped);
}

#[tokio::test]
async fn a_catalog_change_rejects_an_already_queued_unsent_call_without_discarding_the_active_reply()
 {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "before").unwrap();
    let extension = McpExtension::connect(
        config(root.path(), "legacy_queued_change"),
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    let first = read(&extension);
    let ctx = context(root.path());
    let first = tokio::spawn(async move { first.execute(&ctx, json!({"path":"note.txt"})).await });
    until(|| root.path().join("active").exists()).await;
    let second = read(&extension);
    let ctx = context(root.path());
    let second =
        tokio::spawn(async move { second.execute(&ctx, json!({"path":"note.txt"})).await });
    tokio::task::yield_now().await;
    fs::write(root.path().join("catalog_changed"), "changed").unwrap();
    fs::write(root.path().join("release"), "release").unwrap();
    first.await.unwrap().unwrap();
    let error = second.await.unwrap().err().unwrap();
    assert_eq!(error.code, "mcp_catalog_stale");
    assert_eq!(extension.diagnostics().requests_sent, 5);
    assert!(extension.shutdown().await.reaped);
}

#[cfg(feature = "images")]
#[tokio::test]
async fn real_mcp_images_reach_http_after_all_parallel_results_and_replay_without_synthetic_facts()
{
    use aicoder_providers::{ChatClient, ClientConfig, ProxyMode};
    use aicoder_runtime::{
        ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings, resources::ResourceSettings,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    for mode in ["image", "image_alias"] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "image fixture").unwrap();
        let extension =
            McpExtension::connect(config(root.path(), mode), TurnExecutionContext::new())
                .await
                .unwrap();
        let mut extensions = ExtensionRegistry::new();
        extensions.install(extension.extension().unwrap()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for i in 0..3 {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(10), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 8192];
                let end = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(pos) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        break pos + 4;
                    }
                    assert!(bytes.len() < 16384);
                };
                let size = std::str::from_utf8(&bytes[..end])
                    .unwrap()
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                assert!(size < 2 * 1024 * 1024);
                while bytes.len() < end + size {
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                }
                requests.push(
                    serde_json::from_slice::<serde_json::Value>(&bytes[end..end + size]).unwrap(),
                );
                let message = if i == 0 {
                    json!({"role":"assistant","tool_calls":[
                        {"id":"left","type":"function","function":{"name":"fixture__read","arguments":"{\"path\":\"note.txt\"}"}},
                        {"id":"right","type":"function","function":{"name":"fixture__read","arguments":"{\"path\":\"note.txt\"}"}}
                    ]})
                } else {
                    json!({"role":"assistant","content":"received image previews"})
                };
                let body = json!({"id":"fixture","object":"chat.completion","created":1,"model":"vision","choices":[{"index":0,"message":message,"finish_reason":if i==0 {"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}).to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
            requests
        });
        let mut client = ChatClient::new(ClientConfig {
            base_url: format!("http://{address}/v1"),
            api_key: "fixture-only".into(),
            model: "vision".into(),
            proxy: ProxyMode::Disabled,
            max_retries: 0,
            ..Default::default()
        })
        .unwrap();
        let mut spec = ModelSpec::new("openai-compatible", "vision");
        spec.capabilities.tools = true;
        spec.capabilities.images = true;
        client.register_model(spec).unwrap();
        let runtime = Runtime::load_with_environment(
            root.path(),
            RuntimeOverrides {
                settings: Settings {
                    resources: ResourceSettings {
                        enabled: Setting::Value(false),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                session_dir: Setting::Value(state.path().into()),
                ..Default::default()
            },
            ConfigDiscovery {
                enabled: false,
                ..Default::default()
            },
            Arc::new(std::collections::BTreeMap::new()),
        )
        .unwrap();
        let executor = runtime
            .executor_builder_with_extensions(Arc::new(client), Arc::new(extensions.finish()))
            .unwrap()
            .approval(AllowAllApproval)
            .config(TurnExecutionConfig {
                stream: false,
                ..Default::default()
            })
            .build()
            .unwrap();
        let agent = Agent::new(executor, AgentConfig::new("vision"));
        let repo = runtime.sessions().unwrap();
        let session = repo.create(root.path()).unwrap();
        let selection = || SessionSelection::Existing(session.metadata().id.clone());
        agent
            .run_with_session_outcome(
                &repo,
                selection(),
                "inspect the images",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
        let first = repo.open(&session.metadata().id).unwrap();
        let canonical = first
            .messages()
            .iter()
            .map(|m| m.message.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            canonical
                .iter()
                .filter(|m| m.role == MessageRole::User)
                .count(),
            1
        );
        assert_eq!(
            canonical
                .iter()
                .flat_map(|m| &m.content)
                .filter(|b| matches!(
                    b,
                    ContentBlock::Image {
                        source: ImageSource::Artifact { .. }
                    }
                ))
                .count(),
            2
        );
        let serialized = serde_json::to_string(&canonical).unwrap();
        assert!(!serialized.contains("_tool_image_projection"));
        assert!(!serialized.contains("DO_NOT_PERSIST_PRIVATE_RESULT_META"));
        assert!(!serialized.contains("base64"));
        for block in canonical.iter().flat_map(|m| &m.content) {
            if let ContentBlock::Image {
                source: ImageSource::Artifact { reference },
            } = block
            {
                assert_eq!(
                    reference.media_type,
                    if mode == "image_alias" {
                        " IMAGE/PNG ; profile=fixture"
                    } else {
                        "image/png"
                    }
                );
            }
        }
        for message in canonical.iter().filter(|m| m.role == MessageRole::Tool) {
            let result = message.tool_result().unwrap().1;
            assert!(
                result["output"]["structuredContent"]["calls"]
                    .as_u64()
                    .unwrap()
                    <= 2
            );
        }
        agent
            .run_with_session_outcome(
                &repo,
                selection(),
                "look again",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
        let requests = server.await.unwrap();
        for request in &requests[1..] {
            let messages = request["messages"].as_array().unwrap();
            let start = messages.iter().position(|m| m["role"] == "tool").unwrap();
            assert_eq!(messages[start + 1]["role"], "tool");
            assert_eq!(messages[start + 2]["role"], "user");
            let image_parts = messages[start + 2]["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["type"] == "image_url")
                .collect::<Vec<_>>();
            assert_eq!(image_parts.len(), 2);
            for image in image_parts {
                use base64::Engine;
                let data = image["image_url"]["url"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("data:image/jpeg;base64,")
                    .unwrap();
                let jpeg = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap();
                assert_eq!(image::load_from_memory(&jpeg).unwrap().width(), 2);
            }
            for message in &messages[start..start + 2] {
                let result: serde_json::Value =
                    serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
                assert_eq!(
                    result["_tool_image_projection"][0]["status"],
                    "normalized_preview"
                );
            }
        }
        let final_session = repo.open(&session.metadata().id).unwrap();
        assert_eq!(
            final_session
                .messages()
                .iter()
                .filter(|m| m.message.role == MessageRole::Tool)
                .count(),
            2
        );
        assert!(extension.shutdown().await.reaped);
    }
}

#[cfg(feature = "images")]
#[tokio::test]
async fn nonvision_and_corrupt_tool_images_preserve_completion_without_reexecuting_the_call() {
    use aicoder_runtime::{
        ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings, resources::ResourceSettings,
    };
    struct ImageProvider(bool, Mutex<Vec<ModelRequest>>);
    #[async_trait]
    impl ModelProvider for ImageProvider {
        fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
            let mut spec = ModelSpec::new("fixture", model);
            spec.capabilities.tools = true;
            spec.capabilities.images = self.0;
            Ok(spec)
        }
        async fn generate(
            &self,
            request: ModelRequest,
            _: Arc<dyn ModelEventSink>,
            _: TurnExecutionContext,
        ) -> Result<ModelResponse, ProviderError> {
            request.validate(&self.model_spec(&request.model)?)?;
            let has_result = request.messages.iter().any(|m| m.role == MessageRole::Tool);
            let mut message = Message::text(MessageRole::Assistant, "done");
            if !has_result {
                message.content = vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "image".into(),
                        name: "fixture__read".into(),
                        arguments: json!({"path":"note.txt"}),
                    },
                }];
            }
            self.1.lock().unwrap().push(request.clone());
            Ok(ModelResponse {
                id: None,
                model: request.model,
                message,
                stop_reason: if has_result {
                    StopReason::Complete
                } else {
                    StopReason::ToolCalls
                },
                usage: ModelUsage::zero(),
            })
        }
    }
    for (vision, mode, expected) in [
        (false, "image", "model_without_vision"),
        (true, "media", "invalid_image"),
        (true, "image_unsupported_mime", "invalid_image"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "fixture").unwrap();
        let extension =
            McpExtension::connect(config(root.path(), mode), TurnExecutionContext::new())
                .await
                .unwrap();
        let mut extensions = ExtensionRegistry::new();
        extensions.install(extension.extension().unwrap()).unwrap();
        let runtime = Runtime::load_with_environment(
            root.path(),
            RuntimeOverrides {
                settings: Settings {
                    resources: ResourceSettings {
                        enabled: Setting::Value(false),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                session_dir: Setting::Value(state.path().into()),
                ..Default::default()
            },
            ConfigDiscovery {
                enabled: false,
                ..Default::default()
            },
            Arc::new(std::collections::BTreeMap::new()),
        )
        .unwrap();
        let provider = Arc::new(ImageProvider(vision, Mutex::new(vec![])));
        let agent = Agent::new(
            runtime
                .executor_builder_with_extensions(provider.clone(), Arc::new(extensions.finish()))
                .unwrap()
                .approval(AllowAllApproval)
                .build()
                .unwrap(),
            AgentConfig::new("m"),
        );
        let repo = runtime.sessions().unwrap();
        let session = repo.create(root.path()).unwrap();
        agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "inspect",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
        let saved = repo.open(&session.metadata().id).unwrap();
        let saved_messages = saved.messages();
        let tool = saved_messages
            .iter()
            .find(|m| m.message.role == MessageRole::Tool)
            .unwrap();
        let (_, result, error) = tool.message.tool_result().unwrap();
        assert!(!error);
        assert_eq!(result["output"]["operation_completed"], true);
        assert_eq!(result["output"]["structuredContent"]["calls"], 1);
        assert_eq!(result["output"]["delivery_complete"], true); // Raw archive captured; preview may fail independently.
        assert!(tool.message.content.iter().any(|b| matches!(
            b,
            ContentBlock::Image {
                source: ImageSource::Artifact { .. }
            }
        )));
        assert!(result.get("_tool_image_projection").is_none());
        {
            let requests = provider.1.lock().unwrap();
            assert_eq!(requests.len(), 2);
            let projected = requests[1]
                .messages
                .iter()
                .find(|m| m.role == MessageRole::Tool)
                .unwrap();
            assert!(
                !projected
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Image { .. }))
            );
            assert_eq!(
                projected.tool_result().unwrap().1["_tool_image_projection"][0]["status"],
                expected
            );
        }
        assert!(extension.shutdown().await.reaped);
    }
}

#[tokio::test]
async fn mcp_nonimage_mime_in_an_image_block_reports_incomplete_delivery_without_forged_images() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("note.txt"), "fixture").unwrap();
    let extension = McpExtension::connect(
        config(root.path(), "image_invalid_mime"),
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    let store = Arc::new(MemoryArtifactStore::default());
    let result = read(&extension)
        .execute(
            &context(root.path()).with_artifact_store(store),
            json!({"path":"note.txt"}),
        )
        .await
        .unwrap();
    assert!(result.artifacts.is_empty());
    assert!(result.truncated);
    assert_eq!(result.output["operation_completed"], true);
    assert_eq!(result.output["delivery_complete"], false);
    assert_eq!(
        result.output["capture_errors"][0]["code"],
        "invalid_image_mime"
    );
    assert_eq!(result.output["structuredContent"]["calls"], 1);
    assert!(extension.shutdown().await.reaped);
}
