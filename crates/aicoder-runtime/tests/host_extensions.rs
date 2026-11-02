use aicoder_core::{TurnExecutionContext, extensions::Extension, tools::ToolRegistry};
use aicoder_runtime::{host_extensions::*, resources::ResourceSettings, *};
use std::{collections::BTreeMap, path::Path, sync::Arc};
fn runtime(root: &Path) -> Runtime {
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
            session_dir: Setting::Clear,
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::<String, String>::new()),
    )
    .unwrap()
}
#[tokio::test]
async fn empty_host_activation_is_explicit_and_never_changes_default_builders() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    assert!(runtime.extensions().is_none());
    let manifest = HostExtensionManifest {
        schema_version: 1,
        entries: Vec::new(),
    };
    let activation = runtime
        .activate_extensions(&manifest, TurnExecutionContext::new())
        .await
        .unwrap();
    assert!(activation.extensions().catalog().registrations.is_empty());
    activation.shutdown().await.unwrap();
}
#[test]
fn host_launch_files_reject_relative_programs_duplicates_and_unbounded_metadata() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("host.json");
    for value in [
        serde_json::json!({"schema_version":1,"entries":[{"id":"one","kind":"process","program":"relative"}]}),
        serde_json::json!({"schema_version":2,"entries":[]}),
        serde_json::json!({"schema_version":1,"entries":[],"api_key":"private-must-not-be-in-error"}),
    ] {
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let result = HostExtensionManifest::read(&path);
        assert!(result.is_err());
        assert!(
            !result
                .err()
                .unwrap()
                .to_string()
                .contains("private-must-not-be-in-error")
        );
    }
}
#[test]
fn launch_debug_does_not_echo_private_arguments_or_paths() {
    let entry = HostExtensionEntry {
        id: "one".into(),
        kind: HostExtensionKind::Process,
        program: "/private-executable".into(),
        arguments: vec!["private-argument".into()],
        environment_references: BTreeMap::new(),
        aliases: BTreeMap::new(),
        capabilities: BTreeMap::new(),
    };
    assert!(!format!("{entry:?}").contains("private"));
}
#[test]
fn combined_native_sets_validate_collisions_without_publishing_partial_changes() {
    use aicoder_core::extensions::ExtensionRegistry;
    let mut a = ExtensionRegistry::new();
    a.install(Extension::new("one", "1").unwrap()).unwrap();
    let a = a.finish();
    let mut b = ExtensionRegistry::new();
    b.install(Extension::new("two", "1").unwrap()).unwrap();
    let b = b.finish();
    assert_eq!(a.combine(&b).unwrap().catalog().extensions.len(), 2);
    assert!(a.combine(&a).is_err());
    assert_eq!(a.catalog().extensions.len(), 1);
    a.validate_tools(&ToolRegistry::default()).unwrap();
}
#[cfg(not(feature = "process-extensions"))]
#[test]
fn compiled_out_transports_fail_before_launch() {
    let manifest = HostExtensionManifest {
        schema_version: 1,
        entries: vec![HostExtensionEntry {
            id: "host".into(),
            kind: HostExtensionKind::Process,
            program: "/must-not-run".into(),
            arguments: Vec::new(),
            environment_references: BTreeMap::new(),
            aliases: BTreeMap::new(),
            capabilities: BTreeMap::new(),
        }],
    };
    assert!(manifest.validate().is_err());
}
#[cfg(all(feature = "mcp-fixtures", target_os = "linux"))]
mod fixtures {
    use super::*;
    use aicoder_core::{model::*, tools::*, *};
    use async_trait::async_trait;
    use serde_json::json;
    use std::{fs, path::PathBuf, sync::Mutex, time::Duration};
    fn entry(root: &Path, id: &str, kind: HostExtensionKind, mode: &str) -> HostExtensionEntry {
        HostExtensionEntry {
            id: id.into(),
            kind,
            program: PathBuf::from(if kind == HostExtensionKind::Process {
                env!("CARGO_BIN_EXE_aicoder-process-fixture")
            } else {
                env!("CARGO_BIN_EXE_aicoder-mcp-fixture")
            }),
            arguments: vec![
                mode.into(),
                root.join(format!("{id}.pid")).to_string_lossy().to_string(),
            ],
            environment_references: BTreeMap::new(),
            aliases: BTreeMap::new(),
            capabilities: BTreeMap::new(),
        }
    }
    async fn until(mut check: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !check() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    fn dead(pid: &str) -> bool {
        fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |s| {
            s.rsplit_once(')')
                .is_some_and(|(_, s)| s.trim_start().starts_with('Z'))
        })
    }
    struct Model {
        tool: String,
        requests: Mutex<Vec<ModelRequest>>,
    }
    #[async_trait]
    impl ModelProvider for Model {
        fn model_spec(&self, name: &str) -> Result<ModelSpec, ProviderError> {
            let mut spec = ModelSpec::new("offline", name);
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
            let call = request.messages.last().unwrap().role == MessageRole::User;
            let response = ModelResponse {
                id: None,
                model: request.model,
                message: if call {
                    Message {
                        role: MessageRole::Assistant,
                        content: vec![ContentBlock::ToolCall {
                            call: ModelToolCall {
                                id: "host".into(),
                                name: self.tool.clone(),
                                arguments: json!({"path":"note.txt","content":"approved"}),
                            },
                        }],
                    }
                } else {
                    Message::text(MessageRole::Assistant, "done")
                },
                stop_reason: if call {
                    StopReason::ToolCalls
                } else {
                    StopReason::Complete
                },
                usage: ModelUsage::zero(),
            };
            emit_response(&response, sink.as_ref());
            Ok(response)
        }
    }
    #[tokio::test]
    async fn process_and_mcp_install_in_normal_runtime_and_keep_approvals_and_reload_snapshots() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("note.txt"), "before").unwrap();
        let runtime = runtime(root.path());
        let manifest = HostExtensionManifest {
            schema_version: 1,
            entries: vec![
                entry(root.path(), "proc", HostExtensionKind::Process, "normal"),
                entry(root.path(), "mcp", HostExtensionKind::McpStdio, "modern"),
            ],
        };
        let activation = runtime
            .activate_extensions(&manifest, TurnExecutionContext::new())
            .await
            .unwrap();
        let runtime = runtime.with_extensions(activation.extensions()).unwrap();
        let provider = Arc::new(Model {
            tool: "mcp__write".into(),
            requests: Mutex::new(Vec::new()),
        });
        let agent = Agent::new(
            runtime
                .executor_builder(provider.clone())
                .unwrap()
                .approval(DenyAllApproval)
                .build()
                .unwrap(),
            runtime.agent_config(),
        );
        let result = agent
            .run("deny actual host write", Arc::new(()))
            .await
            .unwrap();
        assert_eq!(result.execution_result.tool_calls, 1);
        assert_eq!(
            fs::read_to_string(root.path().join("note.txt")).unwrap(),
            "before"
        );
        let model = Arc::new(Model {
            tool: "mcp__write".into(),
            requests: Mutex::new(Vec::new()),
        });
        let reloaded = runtime.reload_resources().unwrap();
        let agent = Agent::new(
            reloaded
                .executor_builder(model.clone())
                .unwrap()
                .approval(AllowAllApproval)
                .build()
                .unwrap(),
            reloaded.agent_config(),
        );
        let result = agent
            .run("approved host write", Arc::new(()))
            .await
            .unwrap();
        assert_eq!(result.execution_result.tool_calls, 1);
        assert_eq!(
            fs::read_to_string(root.path().join("note.txt")).unwrap(),
            "approved"
        );
        let definitions = model.requests.lock().unwrap()[0].tools.clone();
        assert!(definitions.iter().any(|t| t.name == "proc__read"));
        assert!(definitions.iter().any(|t| t.name == "mcp__write"));
        activation.shutdown().await.unwrap();
        for id in ["proc", "mcp"] {
            let pid = fs::read_to_string(root.path().join(format!("{id}.pid"))).unwrap();
            until(|| dead(&pid)).await;
        }
    }
    #[tokio::test]
    async fn dropping_activation_invalidates_retained_tools_and_reaps_owners() {
        let root = tempfile::tempdir().unwrap();
        let runtime = runtime(root.path());
        let activation = runtime
            .activate_extensions(
                &HostExtensionManifest {
                    schema_version: 1,
                    entries: vec![entry(
                        root.path(),
                        "mcp",
                        HostExtensionKind::McpStdio,
                        "modern",
                    )],
                },
                TurnExecutionContext::new(),
            )
            .await
            .unwrap();
        let snapshot = activation.extensions();
        let retained = runtime.with_extensions(snapshot).unwrap();
        let pid = fs::read_to_string(root.path().join("mcp.pid")).unwrap();
        drop(activation);
        until(|| dead(&pid)).await;
        let provider = Arc::new(Model {
            tool: "mcp__write".into(),
            requests: Mutex::new(Vec::new()),
        });
        let agent = Agent::new(
            retained
                .executor_builder(provider)
                .unwrap()
                .approval(AllowAllApproval)
                .build()
                .unwrap(),
            retained.agent_config(),
        );
        let result = agent.run("closed owner", Arc::new(())).await.unwrap();
        let (_, value, error) = result
            .execution_result
            .messages
            .iter()
            .find_map(Message::tool_result)
            .unwrap();
        assert!(error, "{value}");
        assert!(!root.path().join("note.txt").exists());
    }
    #[tokio::test]
    async fn failed_second_startup_cleans_first_owner_and_publishes_no_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let runtime = runtime(root.path());
        let mut bad = entry(root.path(), "bad", HostExtensionKind::McpStdio, "modern");
        bad.program = "/not-an-executable".into();
        let result = runtime
            .activate_extensions(
                &HostExtensionManifest {
                    schema_version: 1,
                    entries: vec![
                        entry(root.path(), "proc", HostExtensionKind::Process, "normal"),
                        bad,
                    ],
                },
                TurnExecutionContext::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(runtime.extensions().is_none());
        let pid = fs::read_to_string(root.path().join("proc.pid")).unwrap();
        until(|| dead(&pid)).await;
    }
    #[tokio::test]
    async fn all_credential_references_are_resolved_before_first_launch_without_leaking_values() {
        let root = tempfile::tempdir().unwrap();
        let runtime = runtime(root.path());
        let mut second = entry(root.path(), "second", HostExtensionKind::Process, "normal");
        second
            .environment_references
            .insert("TOKEN".into(), "MISSING_HOST_REF".into());
        let result = runtime
            .activate_extensions(
                &HostExtensionManifest {
                    schema_version: 1,
                    entries: vec![
                        entry(root.path(), "first", HostExtensionKind::Process, "normal"),
                        second,
                    ],
                },
                TurnExecutionContext::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(!root.path().join("first.pid").exists());
    }
}
