use aicoder_core::{extensions::*, model::*, session::*, ui::*, *};
use aicoder_runtime::{
    ConfigDiscovery, ContextSettings, Runtime, RuntimeOverrides, Setting, Settings,
    resources::ResourceSettings,
};
use async_trait::async_trait;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Summary(AtomicUsize);
#[async_trait]
impl ModelProvider for Summary {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.context_tokens = Some(65536);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        assert!(request.tools.is_empty());
        self.0.fetch_add(1, Ordering::SeqCst);
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(
                MessageRole::Assistant,
                serde_json::to_string(&aicoder_core::summary::ConversationSummary {
                    notes: vec!["Previously discussed the bounded offline fixture".into()],
                    ..Default::default()
                })
                .unwrap(),
            ),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Choose;
#[async_trait]
impl ContextHook for Choose {
    async fn context(
        &self,
        _: ContextHookInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<Option<String>> {
        let UiAnswer::Text { text } = cancellation
            .ui()
            .ask(UiPrompt::Input {
                title: "Summary context".into(),
                initial: String::new(),
                multiline: false,
                max_bytes: 128,
            })
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(format!("Host selected summary context: {text}")))
    }
}
#[tokio::test]
async fn manual_summary_retains_native_ui_hooks_and_raw_canonical_history() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::load_with_environment(
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                model: Setting::Value("offline".into()),
                context: ContextSettings {
                    window_tokens: Setting::Value(65536),
                    preserve_recent_tokens: Setting::Value(1024),
                    ..Default::default()
                },
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
    .unwrap();
    let mut extension = Extension::new("summary-ui", "1").unwrap();
    let mut spec = HookSpec::new("choose");
    spec.timeout = Duration::from_secs(10);
    extension.context_hook(spec, Arc::new(Choose)).unwrap();
    let mut registry = ExtensionRegistry::new();
    registry.install(extension).unwrap();
    let provider = Arc::new(Summary(AtomicUsize::new(0)));
    let agent = runtime
        .summary_agent_with_extensions(provider.clone(), Arc::new(registry.finish()))
        .unwrap();
    let repository = Arc::new(MemorySessionRepository::new());
    let mut session = repository.create(root.path()).unwrap();
    for i in 0..12 {
        repository
            .append(
                &mut session,
                Message::text(
                    if i % 2 == 0 {
                        MessageRole::User
                    } else {
                        MessageRole::Assistant
                    },
                    format!("Canonical-{i} {}", "history ".repeat(800)),
                ),
            )
            .unwrap();
    }
    let before = repository
        .open(&session.metadata().id)
        .unwrap()
        .selected_branch()
        .chat_messages();
    let id = session.metadata().id.clone();
    let host = UiHost::interactive();
    let context = TurnExecutionContext::new().with_ui(host.client("host").unwrap());
    let repo = repository.clone();
    let worker_id = id.clone();
    let worker = tokio::spawn(async move {
        agent
            .compact_session(repo.as_ref(), &worker_id, context)
            .await
    });
    let q = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = host.snapshot();
            if let Some(q) = snapshot.questions.first() {
                break q.clone();
            }
            host.wait(snapshot.revision).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(q.owner, "summary-ui");
    host.answer(
        q.id,
        UiAnswer::Text {
            text: "keep the coding goal".into(),
        },
    )
    .unwrap();
    let report = worker.await.unwrap();
    assert!(report.succeeded(), "{report:?}");
    assert!(report.checkpoint_entry_id.is_some());
    assert_eq!(report.attempts, 1);
    assert_eq!(provider.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        repository
            .open(&id)
            .unwrap()
            .selected_branch()
            .chat_messages(),
        before
    );
    assert!(host.snapshot().questions.is_empty());
    host.close();
}
