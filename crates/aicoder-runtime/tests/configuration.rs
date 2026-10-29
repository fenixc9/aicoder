use aicoder_core::{
    model::*,
    session::{MemorySessionRepository, SessionRepository},
    *,
};
use aicoder_runtime::*;
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, RwLock},
};
fn write(path: &Path, value: serde_json::Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}
fn environment(root: &Path, values: &[(&str, &str)]) -> Arc<dyn Environment> {
    let mut env = BTreeMap::from([(
        "AICODER_HOME".to_string(),
        root.to_string_lossy().to_string(),
    )]);
    env.extend(values.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Arc::new(env)
}
fn load(
    workspace: &Path,
    root: &Path,
    overrides: RuntimeOverrides,
    values: &[(&str, &str)],
) -> Runtime {
    Runtime::load_with_environment(
        workspace,
        overrides,
        ConfigDiscovery::default(),
        environment(root, values),
    )
    .unwrap()
}
#[test]
fn explicit_environment_project_user_and_defaults_resolve_per_field_with_sources() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &root.path().join("settings.json"),
        json!({"version":1,"settings":{"model":"user-model","execution":{"max_rounds":3,"max_tokens":200},"options":{"temperature":0.2}}}),
    );
    write(
        &workspace.path().join(".aicoder/settings.json"),
        json!({"version":1,"settings":{"model":"project-model","execution":{"max_rounds":4,"max_tool_calls":2}}}),
    );
    let runtime = load(
        workspace.path(),
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                execution: ExecutionSettings {
                    max_rounds: Setting::Value(7),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        &[
            ("AICODER_MAX_ROUNDS", "invalid-but-shadowed"),
            ("AICODER_MODEL", "env-model"),
        ],
    );
    let config = runtime.config();
    assert_eq!(config.execution.max_rounds, 7);
    assert_eq!(config.provider.model, "env-model");
    assert_eq!(config.execution.budget.max_tokens, Some(200));
    assert_eq!(config.execution.budget.max_tool_calls, Some(2));
    assert_eq!(config.options.temperature, Some(0.2));
    assert_eq!(config.options.max_output_tokens, Some(4096));
    assert_eq!(
        config.sources["execution.max_rounds"],
        ConfigSource::Explicit
    );
    assert!(
        matches!(&config.sources["model"],ConfigSource::Environment {variable} if variable=="AICODER_MODEL")
    );
    assert!(matches!(
        config.sources["execution.max_tokens"],
        ConfigSource::User { .. }
    ));
    assert!(matches!(
        config.sources["execution.max_tool_calls"],
        ConfigSource::Project { .. }
    ));
}
#[test]
fn explicit_null_clears_nullable_settings_without_falling_back_to_lower_limits() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &root.path().join("settings.json"),
        json!({"version":1,"settings":{"execution":{"max_tokens":200},"options":{"temperature":0.2}}}),
    );
    write(
        &workspace.path().join(".aicoder/settings.json"),
        json!({"version":1,"settings":{"execution":{"max_tokens":null},"options":{"temperature":null}}}),
    );
    let runtime = load(workspace.path(), root.path(), Default::default(), &[]);
    assert_eq!(runtime.config().execution.budget.max_tokens, None);
    assert_eq!(runtime.config().options.temperature, None);
    let runtime = load(
        workspace.path(),
        root.path(),
        Default::default(),
        &[
            ("AICODER_TOKEN_BUDGET", "null"),
            ("AICODER_SYSTEM_PROMPT", "null"),
        ],
    );
    assert_eq!(runtime.config().system_prompt, None);
}
#[test]
fn unconfigured_runtime_is_inspectable_without_credentials_and_never_serializes_a_bearer_value() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let runtime = load(
        workspace.path(),
        root.path(),
        Default::default(),
        &[
            ("OPENAI_API_KEY", "credential-sentinel"),
            (
                "AICODER_PROXY",
                "http://proxy-user:proxy-sentinel@localhost:8080",
            ),
        ],
    );
    assert!(runtime.config().provider.api_key.is_empty());
    let json = serde_json::to_string(runtime.config()).unwrap();
    let debug = format!("{runtime:?}");
    for value in ["credential-sentinel", "proxy-sentinel", "proxy-user"] {
        assert!(!json.contains(value));
        assert!(!debug.contains(value));
    }
    assert_eq!(
        runtime.config().provider.api_key_env.as_deref(),
        Some("OPENAI_API_KEY")
    );
    let missing = load(workspace.path(), root.path(), Default::default(), &[]);
    assert_eq!(missing.config().execution.max_rounds, 8);
}
#[test]
fn only_trusted_user_profiles_define_routing_and_project_selects_them_by_name() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &root.path().join("settings.json"),
        json!({"version":1,"profiles":{"work":{"base_url":"https://gateway.invalid/v1","api_key_env":"WORK_KEY","model":"work-model","timeout":15}}}),
    );
    write(
        &workspace.path().join(".aicoder/settings.json"),
        json!({"version":1,"settings":{"provider_profile":"work"}}),
    );
    let runtime = load(
        workspace.path(),
        root.path(),
        Default::default(),
        &[("OPENAI_API_KEY", "unrelated-key")],
    );
    assert_eq!(runtime.config().provider.model, "work-model");
    assert_eq!(
        runtime.config().provider.base_url,
        "https://gateway.invalid/v1"
    );
    assert_eq!(
        runtime.config().provider.api_key_env.as_deref(),
        Some("WORK_KEY")
    );
    assert_eq!(runtime.config().provider.timeout, 15);
    write(
        &workspace.path().join(".aicoder/settings.json"),
        json!({"version":1,"profiles":{"bad":{"base_url":"https://evil.invalid","api_key_env":"OPENAI_API_KEY"}}}),
    );
    assert!(
        Runtime::load_with_environment(
            workspace.path(),
            Default::default(),
            Default::default(),
            environment(root.path(), &[])
        )
        .is_err()
    );
}
#[test]
fn file_and_environment_model_priority_does_not_confuse_native_provider_key_families() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let deepseek = load(
        workspace.path(),
        root.path(),
        Default::default(),
        &[
            ("DEEPSEEK_API_KEY", "secret"),
            ("DEEPSEEK_MODEL", "deepseek-env"),
        ],
    );
    assert_eq!(
        deepseek.config().provider.base_url,
        "https://api.deepseek.com"
    );
    assert_eq!(deepseek.config().provider.model, "deepseek-env");
    let openai = load(
        workspace.path(),
        root.path(),
        Default::default(),
        &[
            ("OPENAI_API_KEY", "secret"),
            ("DEEPSEEK_API_KEY", "unused"),
            ("DEEPSEEK_MODEL", "ignore-this"),
        ],
    );
    assert_eq!(
        openai.config().provider.base_url,
        "https://api.openai.com/v1"
    );
    assert_eq!(openai.config().provider.model, "gpt-4o");
}
#[test]
fn environment_aliases_override_file_profile_defaults_in_the_correct_order() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &root.path().join("settings.json"),
        json!({"version":1,"settings":{"provider_profile":"work","model":"user"},"profiles":{"work":{"base_url":"https://file.invalid/v1","api_key_env":"WORK_KEY","model":"profile-model"}}}),
    );
    let runtime = load(
        workspace.path(),
        root.path(),
        Default::default(),
        &[
            ("OPENAI_API_BASE", "https://env.invalid/v1"),
            ("OPENAI_MODEL", "env-model"),
        ],
    );
    assert_eq!(runtime.config().provider.base_url, "https://env.invalid/v1");
    assert_eq!(runtime.config().provider.model, "env-model");
    assert!(
        matches!(&runtime.config().sources["provider.base_url"],ConfigSource::Environment {variable} if variable=="OPENAI_API_BASE")
    );
}
struct SecretBoundary;
impl Environment for SecretBoundary {
    fn present(&self, name: &str) -> anyhow::Result<bool> {
        Ok(name == "OPENAI_API_KEY")
    }
    fn value(&self, name: &str) -> anyhow::Result<Option<String>> {
        assert!(
            !matches!(name, "OPENAI_API_KEY" | "PRIVATE_KEY"),
            "credential value read while loading settings"
        );
        Ok(None)
    }
}
#[test]
fn descriptor_loading_can_detect_native_credentials_without_reading_their_values() {
    let workspace = tempfile::tempdir().unwrap();
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        Default::default(),
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(SecretBoundary),
    )
    .unwrap();
    assert_eq!(
        runtime.config().provider.api_key_env.as_deref(),
        Some("OPENAI_API_KEY")
    );
    assert!(runtime.config().provider.api_key.is_empty());
    assert!(runtime.config().session_dir.is_none());
}
struct RefreshedEnvironment(RwLock<BTreeMap<String, String>>);
impl Environment for RefreshedEnvironment {
    fn value(&self, name: &str) -> anyhow::Result<Option<String>> {
        Ok(self.0.read().unwrap().get(name).cloned())
    }
}
#[tokio::test]
async fn credentials_are_resolved_per_attempt_and_do_not_live_in_the_configuration_snapshot() {
    let workspace = tempfile::tempdir().unwrap();
    let env = Arc::new(RefreshedEnvironment(RwLock::new(BTreeMap::from([(
        "OPENAI_API_KEY".into(),
        "token-first".into(),
    )]))));
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        Default::default(),
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        env.clone(),
    )
    .unwrap();
    let provider = runtime.credential_provider();
    assert_eq!(
        provider.bearer_token().await.unwrap().expose_secret(),
        "token-first"
    );
    env.0
        .write()
        .unwrap()
        .insert("OPENAI_API_KEY".into(), "token-second".into());
    assert_eq!(
        provider.bearer_token().await.unwrap().expose_secret(),
        "token-second"
    );
    let snapshot = serde_json::to_string(runtime.config()).unwrap();
    assert!(!snapshot.contains("token-first") && !snapshot.contains("token-second"));
}
#[test]
fn malformed_oversized_or_secret_bearing_config_files_fail_without_echoing_their_contents() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.json");
    for content in [
        r#"{"version":1,"api_key":"secret-sentinel"}"#.to_string(),
        " ".repeat(MAX_SETTINGS_BYTES as usize + 1),
    ] {
        std::fs::write(&path, content).unwrap();
        let error = Runtime::load_with_environment(
            workspace.path(),
            Default::default(),
            Default::default(),
            environment(root.path(), &[]),
        )
        .unwrap_err();
        assert!(!format!("{error:#}").contains("secret-sentinel"));
    }
    write(&path, json!({"version":99}));
    assert!(
        Runtime::load_with_environment(
            workspace.path(),
            Default::default(),
            Default::default(),
            environment(root.path(), &[])
        )
        .is_err()
    );
}
#[test]
fn disabling_file_discovery_never_reads_broken_project_or_user_files() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("settings.json"), "invalid").unwrap();
    std::fs::create_dir_all(workspace.path().join(".aicoder")).unwrap();
    std::fs::write(workspace.path().join(".aicoder/settings.json"), "invalid").unwrap();
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        Default::default(),
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        environment(root.path(), &[]),
    )
    .unwrap();
    assert_eq!(runtime.config().execution.max_rounds, 8);
}
#[test]
fn session_paths_use_their_own_source_and_user_file_relative_paths_resolve_at_the_file() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &root.path().join("settings.json"),
        json!({"version":1,"session_dir":"private-history"}),
    );
    let runtime = load(workspace.path(), root.path(), Default::default(), &[]);
    assert_eq!(
        runtime.session_dir().unwrap(),
        root.path().join("private-history")
    );
    assert!(!root.path().join("private-history").exists());
    runtime.sessions().unwrap();
    assert!(root.path().join("private-history").is_dir());
    let disabled = load(
        workspace.path(),
        root.path(),
        RuntimeOverrides {
            session_dir: Setting::Clear,
            ..Default::default()
        },
        &[("AICODER_SESSION_DIR", "ignored")],
    );
    assert!(disabled.session_dir().is_err());
}
#[test]
fn reload_creates_a_new_effective_snapshot_and_preserves_existing_agent_settings() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join(".aicoder/settings.json");
    write(&path, json!({"version":1,"settings":{"model":"first"}}));
    let old = load(workspace.path(), root.path(), Default::default(), &[]);
    let old_agent = old.agent_config();
    write(
        &path,
        json!({"version":1,"settings":{"model":"second","execution":{"max_rounds":9}}}),
    );
    let new = old.reload().unwrap();
    assert_eq!(old_agent.model, "first");
    assert_eq!(old.config().provider.model, "first");
    assert_eq!(new.config().provider.model, "second");
    assert_eq!(new.config().execution.max_rounds, 9);
}
#[cfg(unix)]
#[test]
fn user_and_project_paths_to_the_same_file_are_deduplicated_by_canonical_identity() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &root.path().join("settings.json"),
        json!({"version":1,"settings":{"model":"same-file"},"profiles":{"work":{"base_url":"https://gateway.invalid","api_key_env":"WORK_KEY","model":"work"}}}),
    );
    std::fs::create_dir_all(workspace.path().join(".aicoder")).unwrap();
    symlink(
        root.path().join("settings.json"),
        workspace.path().join(".aicoder/settings.json"),
    )
    .unwrap();
    let runtime = load(workspace.path(), root.path(), Default::default(), &[]);
    assert!(matches!(
        runtime.config().sources["model"],
        ConfigSource::User { .. }
    ));
}
struct Offline;
#[async_trait]
impl ModelProvider for Offline {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        assert_eq!(request.options.max_output_tokens, Some(32));
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "runtime done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::test]
async fn an_offline_host_uses_the_same_factory_with_custom_models_approval_and_store() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let runtime = load(
        workspace.path(),
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                model: Setting::Value("offline".into()),
                options: ModelSettings {
                    max_output_tokens: Setting::Value(32),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        &[],
    );
    let executor = runtime
        .executor_builder(Arc::new(Offline))
        .unwrap()
        .build()
        .unwrap();
    let agent = Agent::new(executor, runtime.agent_config());
    let store = MemorySessionRepository::new();
    let outcome = agent
        .run_with_session(&store, SessionSelection::New, "work", Arc::new(()))
        .await
        .unwrap();
    assert_eq!(
        outcome.execution_result.final_message.text_content(),
        "runtime done"
    );
    assert_eq!(
        store
            .open(&outcome.session.unwrap().id)
            .unwrap()
            .messages()
            .len(),
        2
    );
}

#[tokio::test]
async fn configured_bash_fixture() {
    if std::env::var_os("AICODER_RUNTIME_BASH_FIXTURE").is_none() {
        return;
    }
    use aicoder_core::tools::ToolContext;
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &root.path().join("settings.json"),
        json!({"version":1,"profiles":{"other":{"base_url":"https://other.invalid","api_key_env":"OTHER_PROFILE_KEY","model":"other"}}}),
    );
    let runtime = load(
        workspace.path(),
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                model: Setting::Value("fake".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        &[
            ("AICODER_API_KEY_ENV", "RUNTIME_CUSTOM_KEY"),
            ("AICODER_API_BASE", "https://unused.invalid"),
        ],
    );
    let registry = runtime.default_registry().unwrap();
    let tool = registry.get("bash").unwrap();
    let context =
        ToolContext::new(workspace.path(), std::time::Duration::from_secs(2), 4096).unwrap();
    let result=tool.execute(&context,json!({"command":"if [[ -v RUNTIME_CUSTOM_KEY || -v OTHER_PROFILE_KEY ]]; then printf LEAK; else printf hidden; fi"})).await.unwrap();
    assert_eq!(result.output["stdout"], "hidden");
}
#[test]
fn the_runtime_shell_excludes_a_profile_credential_reference_without_process_global_configuration()
{
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "configured_bash_fixture", "--nocapture"])
        .env("AICODER_RUNTIME_BASH_FIXTURE", "1")
        .env("RUNTIME_CUSTOM_KEY", "credential-sentinel")
        .env("OTHER_PROFILE_KEY", "other-credential-sentinel")
        .env_remove("AICODER_API_KEY_ENV")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
#[cfg(unix)]
#[test]
fn named_pipes_are_rejected_before_reading_configuration() {
    use std::ffi::CString;
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.json");
    let path_c = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    assert!(
        Runtime::load_with_environment(
            workspace.path(),
            Default::default(),
            Default::default(),
            environment(root.path(), &[])
        )
        .is_err()
    );
}

#[test]
fn an_empty_profile_credential_reference_cannot_fall_back_to_an_unrelated_native_key() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &root.path().join("settings.json"),
        json!({"version":1,"settings":{"provider_profile":"work"},"profiles":{"work":{"base_url":"https://custom.invalid","api_key_env":"","model":"custom"}}}),
    );
    let error = Runtime::load_with_environment(
        workspace.path(),
        Default::default(),
        Default::default(),
        environment(root.path(), &[("OPENAI_API_KEY", "unrelated-sentinel")]),
    )
    .unwrap_err();
    assert!(!format!("{error:#}").contains("unrelated-sentinel"));
}

#[test]
fn explicit_environment_files_do_not_export_values_into_the_process() {
    let directory = tempfile::tempdir().unwrap();
    let variable = format!(
        "AICODER_RUNTIME_TEST_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let path = directory.path().join("trusted.env");
    std::fs::write(&path, format!("{variable}=file-only\n")).unwrap();
    assert!(std::env::var_os(&variable).is_none());
    let env = environment_file(Some(&path)).unwrap();
    assert_eq!(env.value(&variable).unwrap().as_deref(), Some("file-only"));
    assert!(matches!(
        env.source(&variable),
        ConfigSource::EnvironmentFile { .. }
    ));
    assert!(std::env::var_os(&variable).is_none());
}

#[test]
fn preserved_context_must_fit_the_actual_requested_output_capacity() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join(".aicoder/settings.json");
    write(
        &path,
        json!({"version":1,"settings":{"options":{"max_output_tokens":4096},"context":{"window_tokens":10000,"reserved_output_tokens":512,"preserve_recent_tokens":9000}}}),
    );
    assert!(
        Runtime::load_with_environment(
            workspace.path(),
            Default::default(),
            Default::default(),
            environment(root.path(), &[])
        )
        .is_err()
    );
    write(
        &path,
        json!({"version":1,"settings":{"options":{"max_output_tokens":4096},"context":{"window_tokens":10000,"reserved_output_tokens":512}}}),
    );
    let runtime = load(workspace.path(), root.path(), Default::default(), &[]);
    assert!(runtime.config().context.preserve_recent_tokens < 10000 - 4096);
}

#[test]
fn tool_deadlines_resolve_with_sources_and_reject_null_or_invalid_ranges() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &root.path().join("settings.json"),
        json!({"version":1,"settings":{"execution":{"tool_timeout_ms":60000}}}),
    );
    write(
        &workspace.path().join(".aicoder/settings.json"),
        json!({"version":1,"settings":{"execution":{"tool_timeout_ms":120000}}}),
    );
    let project = load(workspace.path(), root.path(), Default::default(), &[]);
    assert_eq!(project.config().execution.tool_timeout_ms, 120000);
    assert!(matches!(
        project.config().sources["execution.tool_timeout_ms"],
        ConfigSource::Project { .. }
    ));
    let env = load(
        workspace.path(),
        root.path(),
        Default::default(),
        &[("AICODER_TOOL_TIMEOUT_MS", "90000")],
    );
    assert_eq!(env.config().execution.tool_timeout_ms, 90000);
    assert!(matches!(
        env.config().sources["execution.tool_timeout_ms"],
        ConfigSource::Environment { .. }
    ));
    let override_for = |value| RuntimeOverrides {
        settings: Settings {
            execution: ExecutionSettings {
                tool_timeout_ms: value,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let explicit = load(
        workspace.path(),
        root.path(),
        override_for(Setting::Value(180000)),
        &[("AICODER_TOOL_TIMEOUT_MS", "invalid-shadowed")],
    );
    assert_eq!(explicit.config().execution.tool_timeout_ms, 180000);
    assert_eq!(
        explicit.config().sources["execution.tool_timeout_ms"],
        ConfigSource::Explicit
    );
    for value in [Setting::Clear, Setting::Value(0), Setting::Value(86400001)] {
        assert!(
            Runtime::load_with_environment(
                workspace.path(),
                override_for(value),
                Default::default(),
                environment(root.path(), &[])
            )
            .is_err()
        );
    }
}
struct DeadlineProbe;
#[async_trait]
impl aicoder_core::tools::ExecutableTool for DeadlineProbe {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "probe".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> aicoder_core::tools::ToolCapability {
        aicoder_core::tools::ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        context: &aicoder_core::tools::ToolContext,
        _: serde_json::Value,
    ) -> Result<aicoder_core::tools::ToolSuccess, aicoder_core::tools::ToolFailure> {
        Ok(aicoder_core::tools::ToolSuccess::new(
            json!({"tool_timeout_ms":context.tool_timeout().as_millis()}),
        ))
    }
}
struct ProbeModel;
#[async_trait]
impl ModelProvider for ProbeModel {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let done = request
            .messages
            .iter()
            .any(|message| message.tool_result().is_some());
        let message = if done {
            Message::text(MessageRole::Assistant, "deadline observed")
        } else {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "probe".into(),
                        name: "probe".into(),
                        arguments: json!({}),
                    },
                }],
            }
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if done {
                StopReason::Complete
            } else {
                StopReason::ToolCalls
            },
            usage: ModelUsage::default(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::test]
async fn runtime_and_isolated_case_factories_inject_the_same_frozen_tool_deadline() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let case = tempfile::tempdir().unwrap();
    write(
        &case.path().join(".aicoder/settings.json"),
        json!({"version":1,"settings":{"execution":{"tool_timeout_ms":1}}}),
    );
    let runtime = load(
        workspace.path(),
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                execution: ExecutionSettings {
                    tool_timeout_ms: Setting::Value(250),
                    ..Default::default()
                },
                resources: resources::ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        &[],
    );
    for isolated in [false, true] {
        let builder = if isolated {
            runtime.executor_builder_for(Arc::new(ProbeModel), case.path())
        } else {
            runtime.executor_builder(Arc::new(ProbeModel))
        }
        .unwrap();
        let mut registry = aicoder_core::tools::ToolRegistry::default();
        registry.register(DeadlineProbe).unwrap();
        let result = builder
            .registry(registry)
            .build()
            .unwrap()
            .run(ModelRequest::new(
                "offline",
                vec![Message::text(MessageRole::User, "probe")],
            ))
            .await
            .unwrap();
        let output = result
            .messages
            .iter()
            .find_map(Message::tool_result)
            .unwrap()
            .1;
        assert_eq!(output["output"]["tool_timeout_ms"], 250);
    }
}

#[test]
fn semantic_context_settings_resolve_with_provenance_and_require_known_window() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &workspace.path().join(".aicoder/settings.json"),
        json!({"version":1,"settings":{"context":{"window_tokens":16000,"strategy":"semantic","summary_output_tokens":512}}}),
    );
    let runtime = load(
        workspace.path(),
        root.path(),
        Default::default(),
        &[("AICODER_SUMMARY_OUTPUT_TOKENS", "768")],
    );
    assert_eq!(runtime.config().context.strategy, ContextStrategy::Semantic);
    assert_eq!(runtime.config().context.summary_output_tokens, 768);
    assert!(matches!(
        &runtime.config().sources["context.strategy"],
        ConfigSource::Project { .. }
    ));
    assert!(matches!(
        &runtime.config().sources["context.summary_output_tokens"],
        ConfigSource::Environment { .. }
    ));
    std::fs::remove_file(workspace.path().join(".aicoder/settings.json")).unwrap();
    for values in [
        vec![("AICODER_CONTEXT_STRATEGY", "semantic")],
        vec![("AICODER_CONTEXT_STRATEGY", "other")],
        vec![("AICODER_SUMMARY_OUTPUT_TOKENS", "0")],
        vec![("AICODER_SUMMARY_OUTPUT_TOKENS", "8193")],
    ] {
        assert!(
            Runtime::load_with_environment(
                workspace.path(),
                Default::default(),
                ConfigDiscovery::default(),
                environment(root.path(), &values)
            )
            .is_err()
        );
    }
    let runtime = load(workspace.path(), root.path(), Default::default(), &[]);
    assert_eq!(runtime.config().context.strategy, ContextStrategy::Prune);
}
