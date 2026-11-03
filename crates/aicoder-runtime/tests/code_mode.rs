use aicoder_runtime::{code_mode::CodeModeSettings, *};
use std::{collections::BTreeMap, path::Path, sync::Arc};
fn load(root: &Path, settings: Settings, env: BTreeMap<String, String>) -> anyhow::Result<Runtime> {
    Runtime::load_with_environment(
        root,
        RuntimeOverrides {
            settings,
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(env),
    )
}
#[test]
fn code_mode_is_disabled_even_when_the_engine_is_compiled() {
    let root = tempfile::tempdir().unwrap();
    let runtime = load(root.path(), Settings::default(), BTreeMap::new()).unwrap();
    assert!(!runtime.config().code_mode.enabled);
    assert!(
        runtime
            .default_registry()
            .unwrap()
            .get("codemode")
            .is_none()
    );
}
#[test]
fn code_mode_configuration_obeys_precedence_and_reports_no_raw_values() {
    let root = tempfile::tempdir().unwrap();
    let env = BTreeMap::from([
        (
            "AICODER_CODE_MODE".into(),
            "invalid shadowed setting".into(),
        ),
        ("AICODER_CODE_MODE_TOOLS".into(), "[\"read_file\"]".into()),
    ]);
    let settings = Settings {
        code_mode: CodeModeSettings {
            enabled: Setting::Value(false),
            tools: Setting::Inherit,
            ..Default::default()
        },
        ..Default::default()
    };
    let runtime = load(root.path(), settings, env).unwrap();
    assert!(!runtime.config().code_mode.enabled);
    assert_eq!(
        runtime.config().code_mode.tools.as_deref(),
        Some(["read_file".to_string()].as_slice())
    );
    assert_eq!(
        runtime.config().sources["code_mode.enabled"],
        ConfigSource::Explicit
    );
    for raw in ["not-json-private-value", "[\"read_file\",\"read_file\"]"] {
        let result = load(
            root.path(),
            Settings::default(),
            BTreeMap::from([("AICODER_CODE_MODE_TOOLS".into(), raw.into())]),
        );
        let error = match result {
            Err(error) => error.to_string(),
            Ok(_) => panic!("invalid selection must fail"),
        };
        assert!(!error.contains(raw), "configuration leaked raw input");
    }
}
#[cfg(not(feature = "code-mode"))]
#[test]
fn engine_opt_in_fails_before_runtime_assembly_without_the_feature() {
    let root = tempfile::tempdir().unwrap();
    let result = load(
        root.path(),
        Settings::default(),
        BTreeMap::from([("AICODER_CODE_MODE".into(), "true".into())]),
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("engine-free builds must reject opt-in"),
    };
    assert!(error.to_string().contains("code-mode feature"));
}
#[cfg(feature = "code-mode")]
mod engine {
    use super::*;
    use aicoder_core::{extensions::*, model::*, tools::*, *};
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use std::sync::Mutex;
    struct Model {
        code: String,
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
            let call = request.messages.last().unwrap().role == MessageRole::User;
            self.requests.lock().unwrap().push(request.clone());
            let response = ModelResponse {
                id: None,
                model: request.model,
                message: if call {
                    Message {
                        role: MessageRole::Assistant,
                        content: vec![ContentBlock::ToolCall {
                            call: ModelToolCall {
                                id: "script".into(),
                                name: "codemode".into(),
                                arguments: json!({"code":self.code}),
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
    struct Echo;
    #[async_trait]
    impl ExecutableTool for Echo {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "native.echo".into(),
                description: Some("native echo data".into()),
                parameters: json!({"type":"object"}),
            }
        }
        fn capability(&self) -> ToolCapability {
            ToolCapability::ReadOnly
        }
        async fn execute(&self, _: &ToolContext, args: Value) -> Result<ToolSuccess, ToolFailure> {
            Ok(ToolSuccess::new(args))
        }
    }
    fn runtime(root: &Path) -> Runtime {
        load(
            root,
            Settings {
                code_mode: CodeModeSettings {
                    enabled: Setting::Value(true),
                    ..Default::default()
                },
                ..Default::default()
            },
            BTreeMap::new(),
        )
        .unwrap()
    }
    async fn run(runtime: &Runtime, extension: Extension, code: &str) -> AgentTurnResult {
        let model = Arc::new(Model {
            code: code.into(),
            requests: Mutex::new(Vec::new()),
        });
        let mut extensions = ExtensionRegistry::new();
        extensions.install(extension).unwrap();
        let agent = Agent::new(
            runtime
                .executor_builder_with_extensions(model.clone(), Arc::new(extensions.finish()))
                .unwrap()
                .approval(AllowAllApproval)
                .build()
                .unwrap(),
            runtime.agent_config(),
        );
        let result = agent.run("compose extension", Arc::new(())).await.unwrap();
        assert_eq!(model.requests.lock().unwrap().len(), 2);
        result
    }
    fn body(result: &AgentTurnResult) -> Value {
        result
            .execution_result
            .messages
            .iter()
            .find_map(|m| m.tool_result().map(|(_, body, _)| body.clone()))
            .unwrap()
    }
    #[tokio::test]
    async fn frozen_native_extensions_are_discoverable_and_callable_through_aliases() {
        let root = tempfile::tempdir().unwrap();
        let mut extension = Extension::new("native", "1").unwrap();
        extension.tool(Arc::new(Echo)).unwrap();
        let result = run(&runtime(root.path()), extension,
            "const d=await describeTool('native.echo'); text(d.alias); text((await searchTools('native echo'))[0].name); text(await tools.native_echo({value:7}));"
        ).await;
        let body = body(&result);
        assert_eq!(body["ok"], true, "{body}");
        assert_eq!(
            body["output"]["program"]["items"],
            json!(["native_echo", "native.echo", {"value":7}])
        );
        assert_eq!(result.execution_result.tool_calls, 2);
    }
    #[cfg(all(feature = "mcp-fixtures", target_os = "linux"))]
    #[tokio::test]
    async fn mcp_write_composes_through_frozen_extensions_without_another_agent_loop() {
        use aicoder_runtime::{mcp::*, process_extensions::ProcessConfig};
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("note.txt"), "before").unwrap();
        let mut process = ProcessConfig::new(
            env!("CARGO_BIN_EXE_aicoder-mcp-fixture"),
            root.path(),
            "fixture",
        );
        process.arguments = vec!["modern".into()];
        let mcp = McpExtension::connect(McpConfig::new(process), TurnExecutionContext::new())
            .await
            .unwrap();
        let result = run(&runtime(root.path()), mcp.extension().unwrap(),
            "text((await describeNamespace('fixture')).tools.length); const r=await tools.fixture__write({path:'note.txt',content:'composed'}); text(r);"
        ).await;
        let body = body(&result);
        assert_eq!(body["ok"], true, "{body}");
        assert_eq!(result.execution_result.tool_calls, 2);
        assert_eq!(
            std::fs::read_to_string(root.path().join("note.txt")).unwrap(),
            "composed"
        );
        assert!(mcp.shutdown().await.reaped);
    }
}

#[test]
fn only_requires_enabled_and_resolves_each_setting_layer_independently() {
    let root = tempfile::tempdir().unwrap();
    let env = BTreeMap::from([("AICODER_CODE_MODE_ONLY".into(), "true".into())]);
    let result = load(root.path(), Settings::default(), env.clone());
    assert!(result.is_err());
    let runtime = load(
        root.path(),
        Settings {
            code_mode: CodeModeSettings {
                only: Setting::Value(false),
                ..Default::default()
            },
            ..Default::default()
        },
        env,
    )
    .unwrap();
    assert!(!runtime.config().code_mode.only);
    assert_eq!(
        runtime.config().sources["code_mode.only"],
        ConfigSource::Explicit
    );
}
#[cfg(feature = "code-mode")]
#[test]
fn only_exposes_one_root_tool_while_preserving_the_leaf_registry() {
    let root = tempfile::tempdir().unwrap();
    let runtime = load(
        root.path(),
        Settings {
            code_mode: CodeModeSettings {
                enabled: Setting::Value(true),
                only: Setting::Value(true),
                ..Default::default()
            },
            ..Default::default()
        },
        BTreeMap::new(),
    )
    .unwrap();
    struct Provider;
    #[async_trait::async_trait]
    impl aicoder_core::ModelProvider for Provider {
        fn model_spec(
            &self,
            model: &str,
        ) -> Result<aicoder_core::model::ModelSpec, aicoder_core::model::ProviderError> {
            let mut spec = aicoder_core::model::ModelSpec::new("fixture", model);
            spec.capabilities.tools = true;
            Ok(spec)
        }
        async fn generate(
            &self,
            _: aicoder_core::model::ModelRequest,
            _: Arc<dyn aicoder_core::model::ModelEventSink>,
            _: aicoder_core::TurnExecutionContext,
        ) -> Result<aicoder_core::model::ModelResponse, aicoder_core::model::ProviderError>
        {
            panic!("assembly must not invoke a model")
        }
    }
    let executor = runtime
        .executor_builder(Arc::new(Provider))
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        executor
            .model_tool_definitions()
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        vec!["codemode"]
    );
    assert!(executor.tool_registry().get("read_file").is_some());
    assert!(executor.tool_registry().get("image_output").is_some());
}
