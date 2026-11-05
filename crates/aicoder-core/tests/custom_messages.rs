use aicoder_core::{events::*, extensions::*, model::*, session::*, *};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[derive(Default)]
struct Provider {
    requests: Mutex<Vec<ModelRequest>>,
    images: bool,
    window: Option<u64>,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", model);
        s.capabilities.images = self.images;
        s.context_tokens = Some(self.window.unwrap_or(16384));
        Ok(s)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        assert!(
            request
                .messages
                .iter()
                .all(|m| m.role != MessageRole::Custom)
        );
        self.requests.lock().unwrap().push(request.clone());
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(
                MessageRole::Assistant,
                if request
                    .messages
                    .first()
                    .is_some_and(|m| m.text_content().starts_with("Summarize historical"))
                {
                    json!({"goals":[],"constraints":[],"changes":[],"todos":[],"notes":["custom source retained"]}).to_string()
                } else {
                    "answer".into()
                },
            ),
            stop_reason: StopReason::Complete,
            usage: ModelUsage {
                input_tokens: Some(5),
                output_tokens: Some(2),
                total_tokens: Some(7),
                cached_input_tokens: Some(0),
                cache_write_tokens: Some(0),
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
fn custom() -> CustomMessage {
    CustomMessage::text("sender", "wake", "/NOT_A_TEMPLATE custom fact")
        .unwrap()
        .with_details(json!({"not_in_prompt":"PRIVATE_DETAILS"}))
        .unwrap()
}
struct RawNever;
#[async_trait]
impl InputHook for RawNever {
    async fn input(&self, _: InputHookInput<'_>, _: ExtensionCancellation) -> Result<InputAction> {
        panic!("custom messages must not run raw input hooks")
    }
}
struct Startup(Arc<AtomicUsize>);
#[async_trait]
impl BeforeStartHook for Startup {
    async fn before_start(
        &self,
        input: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        let Some(RunTrigger::Custom(custom)) = input.trigger else {
            panic!("custom trigger missing")
        };
        assert_eq!(custom.source(), "sender");
        assert_eq!(
            custom.details().unwrap()["not_in_prompt"],
            "PRIVATE_DETAILS"
        );
        assert!(input.history.iter().any(|m| m.role == MessageRole::Custom));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Some(BeforeStartAction {
            system_prefix: Some(vec![Message::text(MessageRole::System, "RUNTIME_OVERRIDE")]),
            append: vec![BoundaryEntry::Context {
                kind: "start".into(),
                text: "startup fact".into(),
            }],
            ..Default::default()
        }))
    }
}
fn extensions(start: Arc<AtomicUsize>) -> Arc<ExtensionSet> {
    let mut e = Extension::new("startup", "1").unwrap();
    e.input_hook(HookSpec::new("never"), Arc::new(RawNever))
        .unwrap();
    e.before_start_hook(HookSpec::new("start"), Arc::new(Startup(start)))
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    Arc::new(r.finish())
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(event.clone());
    }
}
#[tokio::test]
async fn custom_triggers_skip_user_input_and_preserve_source_while_startup_sees_custom_details() {
    for saved in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let starts = Arc::new(AtomicUsize::new(0));
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(p.clone())
                .workspace(root.path())
                .extensions(extensions(starts.clone()))
                .build()
                .unwrap(),
            AgentConfig::new("m").system_prompt("base"),
        );
        let events = Arc::new(Events::default());
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let outcome = if saved {
            agent
                .run_prompt_with_session_outcome(
                    &repo,
                    SessionSelection::Existing(session.metadata().id.clone()),
                    custom(),
                    events.clone(),
                    TurnExecutionContext::new(),
                )
                .await
        } else {
            agent
                .run_prompt_outcome(custom(), events.clone(), TurnExecutionContext::new())
                .await
        };
        let result = outcome.into_result().unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(result.execution_result.usage.total_tokens, Some(7));
        assert_eq!(
            result
                .execution_result
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::Custom)
                .count(),
            1
        );
        assert_eq!(
            result
                .execution_result
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::User)
                .count(),
            0
        );
        let requests = p.requests.lock().unwrap();
        assert_eq!(requests[0].messages[0].text_content(), "RUNTIME_OVERRIDE");
        let encoded = serde_json::to_string(&*requests).unwrap();
        assert!(encoded.contains("[aicoder custom sender/wake]"));
        assert!(!encoded.contains("PRIVATE_DETAILS"));
        let raw = events.0.lock().unwrap();
        let mut validator = EventTraceValidator::new();
        for event in raw.iter() {
            validator.observe(event).unwrap();
        }
        validator.finish().unwrap();
        if saved {
            let session = repo.open(&session.metadata().id).unwrap();
            assert!(session.unfinished_turn().is_none());
            assert_eq!(session.messages().len(), 3);
            assert_eq!(session.messages()[0].message.role, MessageRole::Custom);
            assert_eq!(
                session
                    .messages()
                    .iter()
                    .filter(|m| m.message.role == MessageRole::User)
                    .count(),
                0
            );
        }
    }
}
#[tokio::test]
async fn append_only_custom_context_does_not_run_a_model_or_startup_and_restores_without_a_fake_user()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let p = Arc::new(Provider::default());
    let starts = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(starts.clone()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    agent
        .append_custom_message(
            &repo,
            &session.metadata().id,
            custom(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(p.requests.lock().unwrap().is_empty());
    let saved = repo.open(&session.metadata().id).unwrap();
    assert_eq!(saved.entries().len(), 1);
    assert_eq!(saved.messages()[0].message.role, MessageRole::Custom);
    assert!(saved.unfinished_turn().is_none());
    let restored = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    restored
        .run_prompt_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            custom(),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    let saved = repo.open(&session.metadata().id).unwrap();
    assert_eq!(
        saved
            .messages()
            .iter()
            .filter(|m| m.message.role == MessageRole::Custom)
            .count(),
        2
    );
    assert_eq!(
        saved
            .messages()
            .iter()
            .filter(|m| m.message.role == MessageRole::User)
            .count(),
        0
    );
}
#[test]
fn custom_wire_is_an_explicit_extension_and_cannot_be_impersonated_by_plain_messages_or_generic_metadata()
 {
    let custom = custom().with_display(false);
    let data = SessionEntryData::CustomMessage {
        custom: custom.clone(),
    };
    let value = serde_json::to_value(&data).unwrap();
    assert_eq!(value["kind"], "extension");
    assert_eq!(value["namespace"], CUSTOM_MESSAGE_NAMESPACE);
    let restored: SessionEntryData = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(restored.as_message(), Some(custom.message()));
    assert!(matches!(restored,SessionEntryData::CustomMessage{custom}if !custom.display()));
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    assert!(repo.append(&mut session, custom.message().clone()).is_err());
    let raw = SessionEntryData::Extension {
        namespace: CUSTOM_MESSAGE_NAMESPACE.into(),
        data: value["data"].clone(),
    };
    assert!(serde_json::to_value(&raw).is_err());
    assert!(repo.append_entries(&mut session, vec![raw]).is_err());
    let mut invalid = value;
    invalid["data"]["custom"]["message"]["role"] = json!("user");
    assert!(serde_json::from_value::<SessionEntryData>(invalid).is_err());
}
#[tokio::test]
async fn multimodal_custom_messages_project_images_only_for_compatible_models_before_persistence() {
    for images in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let p = Arc::new(Provider {
            images,
            ..Default::default()
        });
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(p.clone())
                .workspace(root.path())
                .build()
                .unwrap(),
            AgentConfig::new("m"),
        );
        let message = CustomMessage::new(
            "sender",
            "image",
            vec![
                ContentBlock::Text {
                    text: "look".into(),
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: "image/png".into(),
                        data: "AAAA".into(),
                    },
                },
            ],
        )
        .unwrap();
        let outcome = agent
            .run_prompt_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                message,
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert_eq!(matches!(outcome, AgentTurnOutcome::Completed(_)), images);
        assert_eq!(p.requests.lock().unwrap().len(), usize::from(images));
        let saved = repo.open(&session.metadata().id).unwrap();
        if images {
            assert_eq!(saved.messages()[0].message.role, MessageRole::Custom);
            assert!(matches!(
                p.requests.lock().unwrap()[0].messages[0].content.last(),
                Some(ContentBlock::Image { .. })
            ));
        } else {
            assert!(saved.entries().is_empty());
        }
    }
}
#[test]
fn custom_envelopes_bound_content_details_and_image_sources() {
    assert!(CustomMessage::text("bad/source", "kind", "text").is_err());
    assert!(custom().with_details(json!("x".repeat(32768))).is_err());
    assert!(CustomMessage::text("s", "k", "x".repeat(MAX_CUSTOM_MESSAGE_BYTES)).is_err());
    assert!(
        CustomMessage::new(
            "s",
            "k",
            vec![ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "x".into(),
                    name: "x".into(),
                    arguments: json!({})
                }
            }]
        )
        .is_err()
    );
    assert!(
        CustomMessage::new(
            "s",
            "k",
            vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://user:password@example.invalid/p.png".into()
                }
            }]
        )
        .is_err()
    );
    assert!(CustomMessage::new("s", "k", vec![]).is_ok());
}
#[tokio::test]
async fn cancelled_or_busy_append_does_not_change_history() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let p = Arc::new(Provider::default());
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let context = TurnExecutionContext::new();
    context.cancel("cancel");
    assert!(
        agent
            .append_custom_message(&repo, &session.metadata().id, custom(), context)
            .await
            .is_err()
    );
    let mut run = repo.open_run(&session.metadata().id).unwrap();
    let _lease = run.acquire(&repo).unwrap();
    assert!(
        agent
            .append_custom_message(
                &repo,
                &session.metadata().id,
                custom(),
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert!(
        repo.open(&session.metadata().id)
            .unwrap()
            .entries()
            .is_empty()
    );
    assert!(p.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn native_context_at_branch_root_is_preserved_outside_a_replaced_runtime_prefix() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::native_context(
                NativeContextSource {
                    extension: "first".into(),
                    hook: "stored".into(),
                    kind: "note".into(),
                },
                "FIRST_RECORDED_CONTEXT",
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let p = Arc::new(Provider::default());
    let starts = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(starts))
            .build()
            .unwrap(),
        AgentConfig::new("m").system_prompt("host prefix"),
    );
    agent
        .run_prompt_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            custom(),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    let requests = p.requests.lock().unwrap();
    assert_eq!(requests[0].messages[0].text_content(), "RUNTIME_OVERRIDE");
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.text_content().contains("FIRST_RECORDED_CONTEXT"))
    );
}

#[tokio::test]
async fn branch_selection_keeps_old_custom_messages_without_including_siblings_in_new_runs() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let p = Arc::new(Provider::default());
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let first = agent
        .append_custom_message(
            &repo,
            &session.metadata().id,
            CustomMessage::text("s", "a", "BRANCH_A").unwrap(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    agent
        .append_custom_message(
            &repo,
            &session.metadata().id,
            CustomMessage::text("s", "b", "SIBLING_B").unwrap(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let mut selected = repo.open(&session.metadata().id).unwrap();
    repo.checkout(&mut selected, first.leaf_id.as_deref())
        .unwrap();
    agent
        .run_prompt_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            CustomMessage::text("s", "c", "BRANCH_C").unwrap(),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    let requests = p.requests.lock().unwrap();
    let body = serde_json::to_string(&requests[0].messages).unwrap();
    assert!(body.contains("BRANCH_A") && body.contains("BRANCH_C") && !body.contains("SIBLING_B"));
    let all = repo.open(&session.metadata().id).unwrap();
    assert!(
        serde_json::to_string(all.entries())
            .unwrap()
            .contains("SIBLING_B")
    );
}
struct WaitStartup(Arc<tokio::sync::Notify>);
#[async_trait]
impl BeforeStartHook for WaitStartup {
    async fn before_start(
        &self,
        _: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        self.0.notify_one();
        std::future::pending().await
    }
}
#[tokio::test]
async fn cancelling_custom_startup_leaves_the_initial_custom_transaction_unwritten() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let p = Arc::new(Provider::default());
    let started = Arc::new(tokio::sync::Notify::new());
    let mut e = Extension::new("wait", "1").unwrap();
    e.before_start_hook(
        HookSpec::new("start"),
        Arc::new(WaitStartup(started.clone())),
    )
    .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let context = TurnExecutionContext::new();
    let worker = context.clone();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let task = tokio::spawn(async move {
        agent
            .run_prompt_with_session_outcome(
                &task_repo,
                SessionSelection::Existing(task_id),
                custom(),
                Arc::new(()),
                worker,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    context.cancel("cancel custom");
    assert!(matches!(task.await.unwrap(), AgentTurnOutcome::Aborted(_)));
    assert!(repo.open(&id).unwrap().entries().is_empty());
    assert!(p.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn custom_context_obeys_known_windows_before_recording_or_triggering() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let p = Arc::new(Provider {
        window: Some(128),
        ..Default::default()
    });
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let large = CustomMessage::text("s", "large", "x".repeat(4096)).unwrap();
    assert!(
        agent
            .append_custom_message(
                &repo,
                &session.metadata().id,
                large.clone(),
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    let outcome = agent
        .run_prompt_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            large,
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(!matches!(outcome, AgentTurnOutcome::Completed(_)));
    assert!(
        repo.open(&session.metadata().id)
            .unwrap()
            .entries()
            .is_empty()
    );
    assert!(p.requests.lock().unwrap().is_empty());
}
struct Submit;
#[async_trait]
impl ExtensionCommand for Submit {
    fn specification(&self) -> CommandSpec {
        CommandSpec {
            name: "wake".into(),
            description: "custom wake".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn execute(
        &self,
        _: CommandRequest<'_>,
        _: ExtensionCancellation,
    ) -> Result<CommandOutput> {
        Ok(CommandOutput::SubmitCustom(custom()))
    }
}
#[tokio::test]
async fn custom_command_results_adopt_the_registered_sender_before_host_submission() {
    let root = tempfile::tempdir().unwrap();
    let mut e = Extension::new("actual-owner", "1").unwrap();
    e.command(Arc::new(Submit), Duration::from_secs(2)).unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let e = r.finish();
    let result = e
        .execute_command(
            "wake",
            CommandRequest {
                context: CommandContext {
                    workspace: root.path(),
                    session: None,
                },
                arguments: &json!({}),
            },
            &TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let Some(AgentPrompt::Custom(custom)) = result.into_prompt() else {
        panic!()
    };
    assert_eq!(custom.source(), "actual-owner");
    assert!(
        custom
            .message()
            .text_content()
            .starts_with("[aicoder custom actual-owner/wake]")
    );
}
#[tokio::test]
async fn semantic_summaries_receive_custom_facts_as_data_without_private_details_or_inline_image_bytes()
 {
    use aicoder_core::{context::*, summary::*};
    let p = Arc::new(Provider::default());
    let image = CustomMessage::new(
        "sender",
        "wake",
        vec![
            ContentBlock::Text {
                text: "/NOT_A_TEMPLATE custom fact".into(),
            },
            ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: "image/png".into(),
                    data: "QUJDRA==".into(),
                },
            },
        ],
    )
    .unwrap()
    .with_details(json!({"private":"PRIVATE_DETAILS"}))
    .unwrap();
    let mut history = vec![
        Message::text(MessageRole::System, "host"),
        image.message().clone(),
    ];
    history.extend((0..8).map(|i| {
        Message::text(
            if i % 2 == 0 {
                MessageRole::User
            } else {
                MessageRole::Assistant
            },
            "x".repeat(1000),
        )
    }));
    history.push(Message::text(MessageRole::User, "newest"));
    let mut config = SummaryConfig::new(ContextWindowConfig {
        max_context_tokens: 1200,
        reserved_output_tokens: 200,
        preserve_recent_tokens: 80,
    });
    config.output_tokens = 128;
    config.max_input_tokens = 1400;
    let compactor = SemanticContextCompactor::new(p.clone(), "m", config).unwrap();
    let result = compactor
        .compact_recorded(
            CompactionRequest {
                input: ContextCompactionInput {
                    model: "m",
                    messages: &history,
                    tools: None,
                    max_output_tokens: None,
                },
                history: &history,
                system_prefix_len: 1,
                force: true,
                token_budget: None,
                archived_messages: 0,
            },
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert!(result.checkpoint.is_some());
    let requests = p.requests.lock().unwrap();
    let body = serde_json::to_string(&requests[0].messages).unwrap();
    assert!(body.contains("NOT_A_TEMPLATE custom fact") && body.contains("custom"));
    assert!(!body.contains("PRIVATE_DETAILS"));
    assert!(!body.contains("QUJDRA=="));
}

#[tokio::test]
async fn custom_control_setup_mismatch_rejects_before_committing_the_trigger() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let p = Arc::new(Provider::default());
    let first = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(Arc::new(AtomicUsize::new(0))))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let (_handle, control) = first
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let second = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(Arc::new(AtomicUsize::new(0))))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let outcome = second
        .run_prompt_with_session_controlled(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            custom(),
            Arc::new(()),
            control,
        )
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
    assert!(
        repo.open(&session.metadata().id)
            .unwrap()
            .entries()
            .is_empty()
    );
    assert!(p.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn declared_host_prefix_is_not_reclassified_by_a_native_label_looking_like_history() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let p = Arc::new(Provider::default());
    let prefix = "[aicoder extension literal/host/rule]\nACTUAL_HOST_PREFIX";
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("m").system_prompt(prefix),
    );
    agent
        .run_prompt_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            custom(),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(
        p.requests.lock().unwrap()[0].messages[0].text_content(),
        prefix
    );
    assert!(
        repo.open(&session.metadata().id)
            .unwrap()
            .messages()
            .iter()
            .all(|m| m.message.role != MessageRole::System)
    );
}
