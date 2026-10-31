use aicoder_core::{extensions::*, model::*, session::*, *};
use aicoder_runtime::{resources::ResourceSettings, *};
use async_trait::async_trait;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
struct Context;
#[async_trait]
impl ContextHook for Context {
    async fn context(
        &self,
        input: ContextHookInput<'_>,
        _: ExtensionCancellation,
    ) -> anyhow::Result<Option<String>> {
        assert!(
            input
                .messages
                .iter()
                .any(|m| m.text_content().contains("PROJECT_RULE"))
        );
        Ok(Some("NATIVE_RULE".into()))
    }
}
struct Provider(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl ModelProvider for Provider {
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
        self.0.lock().unwrap().push(request.clone());
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
#[tokio::test]
async fn native_extensions_compose_with_pinned_project_resources_and_jsonl_history() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("AGENTS.md"), "PROJECT_RULE").unwrap();
    let runtime = Runtime::load_with_environment(
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    global: Setting::Value(false),
                    parents: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Value(state.path().join("sessions")),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::new()),
    )
    .unwrap();
    let mut extension = Extension::new("fixture", "1").unwrap();
    extension
        .context_hook(HookSpec::new("project-aware"), Arc::new(Context))
        .unwrap();
    let mut registry = ExtensionRegistry::new();
    registry.install(extension).unwrap();
    let extensions = Arc::new(registry.finish());
    let requests = Arc::new(Mutex::new(vec![]));
    let agent = Agent::new(
        runtime
            .executor_builder_with_extensions(Arc::new(Provider(requests.clone())), extensions)
            .unwrap()
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let repo = runtime.sessions().unwrap();
    let session = repo.create(root.path()).unwrap();
    for prompt in ["first", "second"] {
        agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                prompt,
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
    }
    for request in requests.lock().unwrap().iter() {
        assert_eq!(
            request
                .messages
                .iter()
                .filter(|m| m.text_content().contains("PROJECT_RULE"))
                .count(),
            1
        );
        assert_eq!(
            request
                .messages
                .iter()
                .filter(|m| m.text_content() == "NATIVE_RULE")
                .count(),
            1
        );
    }
    let saved = repo.open(&session.metadata().id).unwrap();
    assert_eq!(saved.messages().len(), 4);
    assert!(
        !saved
            .messages()
            .iter()
            .any(|m| m.message.text_content().contains("RULE"))
    );
    assert!(saved.entries().iter().any(|e|matches!(&e.data,SessionEntryData::Extension {namespace,data} if namespace=="aicoder.input_context" && data["base"]["files"].is_array() && data["extensions"][0]["id"]=="fixture")));
}
#[tokio::test]
async fn resource_composition_uses_the_declared_prefix_and_keeps_derived_system_context_out_of_it()
{
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("AGENTS.md"), "PROJECT_RULE").unwrap();
    let runtime = Runtime::load_with_environment(
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    global: Setting::Value(false),
                    parents: Setting::Value(false),
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
        Arc::new(BTreeMap::from([(
            "AICODER_HOME".into(),
            state.path().to_string_lossy().into_owned(),
        )])),
    )
    .unwrap();
    let messages = vec![
        Message::text(MessageRole::System, "host"),
        Message::text(MessageRole::System, "derived marker"),
        Message::text(MessageRole::User, "user"),
    ];
    let resources = runtime.resources().unwrap();
    let prepared = resources
        .prepare_with_prefix(root.path(), &messages, 1)
        .await
        .unwrap();
    assert!(
        !prepared
            .system_prefix
            .iter()
            .any(|m| m.text_content().contains("derived marker"))
    );
    assert!(
        prepared
            .system_prefix
            .iter()
            .any(|m| m.text_content().contains("PROJECT_RULE"))
    );
}
