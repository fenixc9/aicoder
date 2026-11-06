use aicoder_core::{events::*, extensions::*, model::*, session::*, tools::*, *};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
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
    tool: bool,
    gate: bool,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", m);
        s.capabilities.tools = true;
        s.context_tokens = Some(65536);
        Ok(s)
    }
    async fn generate(
        &self,
        r: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let n = {
            let mut v = self.requests.lock().unwrap();
            v.push(r.clone());
            v.len()
        };
        if self.gate && n == 1 {
            self.started.notify_one();
            self.release.notified().await;
        }
        let tool = self.tool && n == 1;
        let response = ModelResponse {
            id: None,
            model: r.model,
            message: if tool {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "call".into(),
                            name: "effect".into(),
                            arguments: json!({"owned":true}),
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::Assistant, "RAW_ASSISTANT")
            },
            stop_reason: if tool {
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
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Effect(Arc<AtomicUsize>);
#[async_trait]
impl ExecutableTool for Effect {
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "effect".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess::new(json!({"raw":"RAW_TOOL"})))
    }
}
#[derive(Default)]
struct Hook {
    seen: Mutex<Vec<(MessageOrigin, MessageRole, String)>>,
    fail: Option<MessageRole>,
    tamper: bool,
}
#[async_trait]
impl MessageHook for Hook {
    async fn finalize(
        &self,
        i: MessageHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<Message>> {
        self.seen
            .lock()
            .unwrap()
            .push((i.origin, i.message.role, i.message.text_content()));
        if self.fail == Some(i.message.role) {
            anyhow::bail!("PRIVATE_HOOK_ERROR")
        }
        let mut m = i.message.clone();
        match m.role {
            MessageRole::User => {
                m = Message::text(
                    MessageRole::User,
                    format!("FINAL_USER:{}", m.text_content()),
                )
            }
            MessageRole::Custom => {
                let ContentBlock::Text { text } = m.content.last_mut().unwrap() else {
                    unreachable!()
                };
                *text = format!("FINAL_CUSTOM:{text}");
                if self.tamper {
                    m.content[0] = ContentBlock::Text {
                        text: "[aicoder custom forged/note]\n".into(),
                    }
                }
            }
            MessageRole::System => {
                let ContentBlock::Text { text } = &mut m.content[0] else {
                    unreachable!()
                };
                text.push_str(" FINAL_CONTEXT");
            }
            MessageRole::Assistant => {
                if m.tool_calls().next().is_none() {
                    m = Message::text(MessageRole::Assistant, "FINAL_ASSISTANT");
                }
            }
            MessageRole::Tool => {
                let ContentBlock::ToolResult {
                    content, is_error, ..
                } = &mut m.content[0]
                else {
                    unreachable!()
                };
                if *is_error {
                    content["error"]["message"] = json!("FINAL_TOOL_ERROR");
                } else {
                    content["output"] = json!({"final":"FINAL_TOOL"});
                }
                if self.tamper {
                    content["truncated"] = json!(true);
                }
            }
        }
        Ok(Some(m))
    }
}
fn extensions(h: Arc<dyn MessageHook>) -> Arc<ExtensionSet> {
    let mut e = Extension::new("messages", "1").unwrap();
    e.message_hook(HookSpec::new("finalize"), h).unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    Arc::new(r.finish())
}
fn agent(
    root: &std::path::Path,
    p: Arc<Provider>,
    h: Arc<dyn MessageHook>,
    effect: Arc<AtomicUsize>,
) -> Arc<Agent> {
    let mut tools = ToolRegistry::default();
    tools.register(Effect(effect)).unwrap();
    Arc::new(Agent::new(
        TurnExecutor::builder_from_shared(p)
            .workspace(root)
            .registry(tools)
            .extensions(extensions(h))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    ))
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, e: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(e.clone());
    }
}
#[tokio::test]
async fn fresh_user_tool_and_assistant_are_finalized_once_before_history_and_next_request() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        tool: true,
        ..Default::default()
    });
    let h = Arc::new(Hook::default());
    let effect = Arc::new(AtomicUsize::new(0));
    let a = agent(root.path(), p.clone(), h.clone(), effect.clone());
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let events = Arc::new(Events::default());
    let result = a
        .run_with_session(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "RAW_USER",
            events.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.execution_result.final_message.text_content(),
        "FINAL_ASSISTANT"
    );
    assert_eq!(result.execution_result.usage.total_tokens, Some(14));
    assert_eq!(effect.load(Ordering::SeqCst), 1);
    let requests = p.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.text_content() == "FINAL_USER:RAW_USER")
    );
    assert!(
        serde_json::to_string(&requests[1].messages)
            .unwrap()
            .contains("FINAL_TOOL")
    );
    let saved = repo.open(&session.metadata().id).unwrap();
    assert_eq!(saved.messages().len(), 4);
    assert!(saved.messages().iter().any(|m| {
        m.message
            .tool_result()
            .is_some_and(|(_, c, _)| c["output"]["final"] == "FINAL_TOOL")
    }));
    assert!(
        !serde_json::to_string(saved.entries())
            .unwrap()
            .contains("RAW_TOOL")
    );
    assert_eq!(h.seen.lock().unwrap().len(), 4);
    let raw = events.0.lock().unwrap().clone();
    assert_eq!(
        raw.iter()
            .filter(|e| matches!(e.event, AgentRawEvent::MessageFinalized { .. }))
            .count(),
        4
    );
    let mut trace = EventTraceValidator::new();
    for e in raw.iter() {
        trace.observe(e).unwrap()
    }
    trace.finish().unwrap();
    // Resuming never re-finalizes the four recorded messages.
    a.run_with_session(
        &repo,
        SessionSelection::Existing(session.metadata().id.clone()),
        "new",
        Arc::new(()),
    )
    .await
    .unwrap();
    assert_eq!(h.seen.lock().unwrap().len(), 6);
}
#[tokio::test]
async fn custom_idle_trigger_and_next_turn_preserve_source_details_and_do_not_reprocess_history() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        gate: true,
        ..Default::default()
    });
    let h = Arc::new(Hook::default());
    let a = agent(
        root.path(),
        p.clone(),
        h.clone(),
        Arc::new(AtomicUsize::new(0)),
    );
    let repo = Arc::new(MemorySessionRepository::new());
    let session = repo.create(root.path()).unwrap();
    let id = session.metadata().id.clone();
    let custom = |text: &str| {
        CustomMessage::text("sender", "note", text)
            .unwrap()
            .with_display(false)
            .with_details(json!({"inert":"NOT_PROMPTED"}))
            .unwrap()
    };
    a.append_custom_message(
        repo.as_ref(),
        &id,
        custom("idle"),
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    let (handle, control) = a
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let owned = a.clone();
    let repository = repo.clone();
    let selected = id.clone();
    let task = tokio::spawn(async move {
        owned
            .run_prompt_with_session_controlled(
                repository.as_ref(),
                SessionSelection::Existing(selected),
                "first",
                Arc::new(()),
                control,
            )
            .await
    });
    p.started.notified().await;
    handle
        .submit_custom(CustomDelivery::NextTurn, custom("next"))
        .await
        .unwrap();
    p.release.notify_one();
    task.await.unwrap().into_result().unwrap();
    assert_eq!(a.custom_inbox().pending(&id).len(), 1);
    a.run_prompt_with_session_outcome(
        repo.as_ref(),
        SessionSelection::Existing(id.clone()),
        custom("trigger"),
        Arc::new(()),
        TurnExecutionContext::new(),
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(a.custom_inbox().pending(&id).len(), 0);
    let seen = h.seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|v| v.0 == MessageOrigin::IdleCustom)
            .count(),
        1
    );
    assert_eq!(
        seen.iter()
            .filter(|v| v.0 == MessageOrigin::NextTurn)
            .count(),
        1
    );
    let saved = repo.open(&id).unwrap();
    let custom = saved
        .entries()
        .iter()
        .filter_map(|e| match &e.data {
            SessionEntryData::CustomMessage { custom } => Some(custom),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(custom.len(), 3);
    assert!(custom.iter().all(|c| c.source() == "sender"
        && !c.display()
        && c.message().text_content().contains("FINAL_CUSTOM")
        && c.details().unwrap()["inert"] == "NOT_PROMPTED"));
    assert!(
        !serde_json::to_string(&p.requests.lock().unwrap().last().unwrap().messages)
            .unwrap()
            .contains("NOT_PROMPTED")
    );
}
#[tokio::test]
async fn queued_user_and_custom_inputs_finalize_at_consumption_with_atomic_provenance() {
    for (custom, steer) in [(false, false), (true, false), (false, true), (true, true)] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider {
            gate: true,
            ..Default::default()
        });
        let h = Arc::new(Hook::default());
        let a = agent(
            root.path(),
            p.clone(),
            h.clone(),
            Arc::new(AtomicUsize::new(0)),
        );
        let repo = Arc::new(MemorySessionRepository::new());
        let s = repo.create(root.path()).unwrap();
        let id = s.metadata().id.clone();
        let (handle, control) = a
            .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
            .unwrap();
        let owned = a.clone();
        let repository = repo.clone();
        let task = tokio::spawn(async move {
            owned
                .run_prompt_with_session_controlled(
                    repository.as_ref(),
                    SessionSelection::Existing(id),
                    "first",
                    Arc::new(()),
                    control,
                )
                .await
        });
        p.started.notified().await;
        if custom {
            handle
                .submit_custom(
                    if steer {
                        CustomDelivery::Steer
                    } else {
                        CustomDelivery::FollowUp
                    },
                    CustomMessage::text("sender", "note", "queued").unwrap(),
                )
                .await
                .unwrap();
        } else {
            let message = Message::text(MessageRole::User, "queued");
            if steer {
                handle.steer(message).unwrap();
            } else {
                handle.follow_up(message).unwrap();
            }
        }
        assert_eq!(h.seen.lock().unwrap().len(), 1);
        p.release.notify_one();
        let result = task.await.unwrap().into_result().unwrap();
        if !steer {
            assert_eq!(result.execution_result.usage.total_tokens, Some(14));
        }
        let seen = h.seen.lock().unwrap();
        assert_eq!(
            seen.iter()
                .filter(|s| s.0
                    == if steer {
                        MessageOrigin::Steer
                    } else {
                        MessageOrigin::FollowUp
                    })
                .count(),
            1
        );
        assert!(
            p.requests.lock().unwrap()[1]
                .messages
                .iter()
                .any(|m| m.text_content().contains(if custom {
                    "FINAL_CUSTOM:queued"
                } else {
                    "FINAL_USER:queued"
                }))
        );
    }
}
#[tokio::test]
async fn callback_and_capture_tampering_stop_after_completed_tool_without_repeating_effects() {
    for tamper in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider {
            tool: true,
            ..Default::default()
        });
        let h = Arc::new(Hook {
            fail: (!tamper).then_some(MessageRole::Tool),
            tamper,
            ..Default::default()
        });
        let effect = Arc::new(AtomicUsize::new(0));
        let a = agent(root.path(), p.clone(), h, effect.clone());
        let result = a
            .run_outcome("user", Arc::new(()), TurnExecutionContext::new())
            .await;
        assert!(!matches!(result, AgentTurnOutcome::Completed(_)));
        assert_eq!(p.requests.lock().unwrap().len(), 1);
        assert_eq!(effect.load(Ordering::SeqCst), 1);
        assert_eq!(result.progress().usage.total_tokens, Some(7));
        assert!(result.progress().messages.iter().any(|m| {
            m.tool_result()
                .is_some_and(|(_, c, _)| c["output"]["raw"] == "RAW_TOOL")
        }));
        assert!(!format!("{result:?}").contains("PRIVATE_HOOK_ERROR"));
    }
}
#[tokio::test]
async fn source_rewrites_and_initial_output_budget_reject_before_turn_append_or_provider() {
    for tamper in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let h = Arc::new(Hook {
            tamper,
            ..Default::default()
        });
        let mut builder = TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(h));
        if !tamper {
            builder = builder.config(TurnExecutionConfig {
                budget: RunBudget {
                    max_output_bytes: Some(40),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let a = Agent::new(builder.build().unwrap(), AgentConfig::new("m"));
        let repo = MemorySessionRepository::new();
        let s = repo.create(root.path()).unwrap();
        let result = a
            .run_prompt_with_session_outcome(
                &repo,
                SessionSelection::Existing(s.metadata().id.clone()),
                CustomMessage::text("sender", "note", "value").unwrap(),
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(!matches!(result, AgentTurnOutcome::Completed(_)));
        assert!(p.requests.lock().unwrap().is_empty());
        assert!(repo.open(&s.metadata().id).unwrap().entries().is_empty());
    }
}
struct Contexts;
#[async_trait]
impl BeforeStartHook for Contexts {
    async fn before_start(
        &self,
        _: BeforeStartInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BeforeStartAction>> {
        Ok(Some(BeforeStartAction {
            append: vec![BoundaryEntry::Context {
                kind: "startup".into(),
                text: "START_CONTEXT".into(),
            }],
            ..Default::default()
        }))
    }
}
#[async_trait]
impl BoundaryHook for Contexts {
    async fn boundary(
        &self,
        i: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        Ok(
            (i.kind == BoundaryKind::BeforeSettle).then(|| BoundaryAction {
                append: vec![BoundaryEntry::Context {
                    kind: "settle".into(),
                    text: "SETTLE_CONTEXT".into(),
                }],
                ..Default::default()
            }),
        )
    }
}
#[tokio::test]
async fn startup_and_boundary_context_keep_typed_provenance_and_commit_effective_bodies() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let h = Arc::new(Hook::default());
    let mut e = Extension::new("contexts", "1").unwrap();
    e.message_hook(HookSpec::new("finalize"), h.clone())
        .unwrap();
    e.before_start_hook(HookSpec::new("start"), Arc::new(Contexts))
        .unwrap();
    e.boundary_hook(HookSpec::new("settle"), Arc::new(Contexts))
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let a = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    a.run_with_session(
        &repo,
        SessionSelection::Existing(session.metadata().id.clone()),
        "user",
        Arc::new(()),
    )
    .await
    .unwrap();
    let saved = repo.open(&session.metadata().id).unwrap();
    let native = saved
        .entries()
        .iter()
        .filter_map(|e| match &e.data {
            SessionEntryData::NativeContext { source, message } => Some((source, message)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(native.len(), 2);
    for (source, message) in native {
        source.validate(message).unwrap();
        assert!(message.text_content().ends_with("FINAL_CONTEXT"));
    }
    assert!(
        p.requests.lock().unwrap()[0]
            .messages
            .iter()
            .any(|m| m.text_content().contains("START_CONTEXT FINAL_CONTEXT"))
    );
    let seen = h.seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|v| v.0 == MessageOrigin::Startup)
            .count(),
        1
    );
    assert_eq!(
        seen.iter()
            .filter(|v| v.0 == MessageOrigin::Boundary)
            .count(),
        1
    );
}
struct Wait {
    started: Arc<tokio::sync::Notify>,
    ended: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl MessageHook for Wait {
    async fn finalize(
        &self,
        _: MessageHookInput<'_>,
        c: ExtensionCancellation,
    ) -> Result<Option<Message>> {
        let ended = self.ended.clone();
        tokio::spawn(async move {
            c.cancelled().await;
            ended.notify_one();
        });
        self.started.notify_one();
        std::future::pending().await
    }
}
#[tokio::test]
async fn cancelled_initial_finalization_expires_children_without_turn_or_provider() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let started = Arc::new(tokio::sync::Notify::new());
    let ended = Arc::new(tokio::sync::Notify::new());
    let a = agent(
        root.path(),
        p.clone(),
        Arc::new(Wait {
            started: started.clone(),
            ended: ended.clone(),
        }),
        Arc::new(AtomicUsize::new(0)),
    );
    let repo = Arc::new(MemorySessionRepository::new());
    let s = repo.create(root.path()).unwrap();
    let id = s.metadata().id.clone();
    let context = TurnExecutionContext::new();
    let owned = context.clone();
    let repository = repo.clone();
    let task = tokio::spawn(async move {
        a.run_prompt_with_session_outcome(
            repository.as_ref(),
            SessionSelection::Existing(id),
            "user",
            Arc::new(()),
            owned,
        )
        .await
    });
    started.notified().await;
    context.cancel("cancel finalization");
    let result = task.await.unwrap();
    assert!(matches!(result, AgentTurnOutcome::Aborted(_)));
    assert!(p.requests.lock().unwrap().is_empty());
    assert!(repo.open(&s.metadata().id).unwrap().entries().is_empty());
    tokio::time::timeout(Duration::from_secs(1), ended.notified())
        .await
        .unwrap();
}
struct Journal {
    supported: bool,
    fail: bool,
}
#[async_trait]
impl RunJournal for Journal {
    async fn record_message_finalization(&self, _: &MessageFinalizationRecord) -> Result<()> {
        anyhow::ensure!(self.supported, "unsupported");
        Ok(())
    }
    async fn finalize_message(&self, _: &MessageFinalizationCommit) -> Result<()> {
        anyhow::ensure!(!self.fail, "storage failed");
        Ok(())
    }
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn unsupported_or_failed_generic_journals_do_not_repeat_paid_requests() {
    for supported in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let executor = TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(Arc::new(Hook::default())))
            .build()
            .unwrap();
        let result = executor
            .run_with_journal(
                ModelRequest::new(
                    "m",
                    vec![Message::text(MessageRole::User, "historical input")],
                ),
                Arc::new(()),
                TurnExecutionContext::new(),
                &Journal {
                    supported,
                    fail: true,
                },
            )
            .await;
        assert!(matches!(result, RunOutcome::Interrupted(_)));
        assert_eq!(p.requests.lock().unwrap().len(), usize::from(supported));
        if supported {
            assert_eq!(result.progress().usage.total_tokens, Some(7));
            assert_eq!(
                result.progress().messages.last().unwrap().text_content(),
                "RAW_ASSISTANT"
            );
        }
    }
}
struct ToolWait(Wait);
#[async_trait]
impl MessageHook for ToolWait {
    async fn finalize(
        &self,
        i: MessageHookInput<'_>,
        c: ExtensionCancellation,
    ) -> Result<Option<Message>> {
        if i.message.role == MessageRole::Tool {
            self.0.finalize(i, c).await
        } else {
            Ok(None)
        }
    }
}
#[tokio::test]
async fn cancellation_after_completed_tool_keeps_original_capture_paid_usage_and_cancel_class() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        tool: true,
        ..Default::default()
    });
    let started = Arc::new(tokio::sync::Notify::new());
    let ended = Arc::new(tokio::sync::Notify::new());
    let count = Arc::new(AtomicUsize::new(0));
    let a = agent(
        root.path(),
        p.clone(),
        Arc::new(ToolWait(Wait {
            started: started.clone(),
            ended: ended.clone(),
        })),
        count.clone(),
    );
    let context = TurnExecutionContext::new();
    let owned = context.clone();
    let task = tokio::spawn(async move { a.run_outcome("user", Arc::new(()), owned).await });
    started.notified().await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    context.cancel("after tool completed");
    let outcome = task.await.unwrap();
    assert!(matches!(outcome, AgentTurnOutcome::Aborted(_)));
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert_eq!(outcome.progress().usage.total_tokens, Some(7));
    assert!(outcome.progress().messages.iter().any(|m| {
        m.tool_result()
            .is_some_and(|(_, v, _)| v["output"]["raw"] == "RAW_TOOL")
    }));
    tokio::time::timeout(Duration::from_secs(1), ended.notified())
        .await
        .unwrap();
}
struct FailToolJournal;
#[async_trait]
impl RunJournal for FailToolJournal {
    async fn record_message_finalization(&self, _: &MessageFinalizationRecord) -> Result<()> {
        Ok(())
    }
    async fn finalize_message(&self, c: &MessageFinalizationCommit) -> Result<()> {
        anyhow::ensure!(
            c.finalized.message.role != MessageRole::Tool,
            "tool commit failed"
        );
        Ok(())
    }
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn failed_tool_receipt_retains_completed_original_without_retry_or_finalization_event() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        tool: true,
        ..Default::default()
    });
    let count = Arc::new(AtomicUsize::new(0));
    let a = agent(
        root.path(),
        p.clone(),
        Arc::new(Hook::default()),
        count.clone(),
    );
    let events = Arc::new(Events::default());
    let outcome = a
        .turn_executor()
        .run_with_journal(
            ModelRequest::new("m", vec![Message::text(MessageRole::User, "history")]),
            events.clone(),
            TurnExecutionContext::new(),
            &FailToolJournal,
        )
        .await;
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.progress().usage.total_tokens, Some(7));
    assert!(outcome.progress().messages.iter().any(|m| {
        m.tool_result()
            .is_some_and(|(_, v, _)| v["output"]["raw"] == "RAW_TOOL")
    }));
    assert!(!events.0.lock().unwrap().iter().any(|e| matches!(
        e.event,
        AgentRawEvent::MessageFinalized {
            origin: MessageOrigin::Tool,
            ..
        }
    )));
}
struct Raw;
#[async_trait]
impl InputHook for Raw {
    async fn input(&self, _: InputHookInput<'_>, _: ExtensionCancellation) -> Result<InputAction> {
        Ok(InputAction::Transform("HOOK_INPUT".into()))
    }
}
#[async_trait]
impl InputExpander for Raw {
    async fn expand(&self, i: AgentInput) -> Result<AgentInput> {
        assert_eq!(i.message().text_content(), "HOOK_INPUT");
        Ok(i.with_prompt_text("EXPANDED_INPUT"))
    }
}
#[tokio::test]
async fn raw_hooks_and_expansion_precede_finalization_without_serializing_private_original() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let h = Arc::new(Hook::default());
    let mut e = Extension::new("input", "1").unwrap();
    e.input_hook(HookSpec::new("raw"), Arc::new(Raw)).unwrap();
    e.message_hook(HookSpec::new("finalize"), h.clone())
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let a = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .input_expander(Arc::new(Raw))
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    a.run_with_session(
        &repo,
        SessionSelection::Existing(session.metadata().id.clone()),
        "RAW_PRIVATE_INPUT",
        Arc::new(()),
    )
    .await
    .unwrap();
    assert_eq!(h.seen.lock().unwrap()[0].2, "EXPANDED_INPUT");
    let saved = repo.open(&session.metadata().id).unwrap();
    assert!(
        !serde_json::to_string(saved.entries())
            .unwrap()
            .contains("RAW_PRIVATE_INPUT")
    );
    assert!(
        p.requests.lock().unwrap()[0]
            .messages
            .iter()
            .any(|m| m.text_content() == "FINAL_USER:EXPANDED_INPUT")
    );
}
struct Last;
#[async_trait]
impl MessageHook for Last {
    async fn finalize(
        &self,
        i: MessageHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<Message>> {
        if i.message.role == MessageRole::User {
            assert_eq!(i.original.text_content(), "original");
            assert_eq!(i.message.text_content(), "FINAL_USER:original");
            Ok(Some(Message::text(MessageRole::User, "ORDERED_FINAL_USER")))
        } else {
            Ok(None)
        }
    }
}
#[tokio::test]
async fn ordered_generic_chains_keep_original_immutable_and_feed_each_effective_proposal_forward() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let mut e = Extension::new("ordered", "1").unwrap();
    let mut late = HookSpec::new("last");
    late.priority = 10;
    e.message_hook(late, Arc::new(Last)).unwrap();
    e.message_hook(HookSpec::new("first"), Arc::new(Hook::default()))
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let a = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    a.run("original", Arc::new(())).await.unwrap();
    assert_eq!(
        p.requests.lock().unwrap()[0]
            .messages
            .last()
            .unwrap()
            .text_content(),
        "ORDERED_FINAL_USER"
    );
}
struct OversizeView(bool);
#[async_trait]
impl MessageHook for OversizeView {
    async fn finalize(
        &self,
        i: MessageHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<Message>> {
        Ok((i.message.role == MessageRole::User).then(|| {
            if self.0 {
                Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Image {
                        source: ImageSource::Url {
                            url: "https://example.invalid/image.png".into(),
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::User, "x".repeat(512 * 1024))
            }
        }))
    }
}
#[tokio::test]
async fn transformed_input_capability_and_known_window_fail_before_session_writes() {
    for images in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let a = agent(
            root.path(),
            p.clone(),
            Arc::new(OversizeView(images)),
            Arc::new(AtomicUsize::new(0)),
        );
        let repo = MemorySessionRepository::new();
        let s = repo.create(root.path()).unwrap();
        let result = a
            .run_with_session(
                &repo,
                SessionSelection::Existing(s.metadata().id.clone()),
                "user",
                Arc::new(()),
            )
            .await;
        assert!(result.is_err());
        assert!(p.requests.lock().unwrap().is_empty());
        assert!(repo.open(&s.metadata().id).unwrap().entries().is_empty());
    }
}
struct DeliveryError;
#[async_trait]
impl ToolResultHook for DeliveryError {
    async fn after_tool(
        &self,
        _: ToolResultHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<ToolResultEdit>> {
        Ok(Some(ToolResultEdit {
            is_error: true,
            payload: json!({"code":"delivery_error","message":"RAW_DELIVERY_ERROR"}),
            artifacts: Vec::new(),
        }))
    }
}
#[tokio::test]
async fn specific_result_edits_precede_generic_finalization_and_keep_actual_execution_receipts() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        tool: true,
        ..Default::default()
    });
    let count = Arc::new(AtomicUsize::new(0));
    let mut e = Extension::new("both", "1").unwrap();
    e.message_hook(HookSpec::new("message"), Arc::new(Hook::default()))
        .unwrap();
    e.tool_result_hook(HookSpec::new("result"), Arc::new(DeliveryError))
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let mut tools = ToolRegistry::default();
    tools.register(Effect(count.clone())).unwrap();
    let a = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .registry(tools)
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let result = a
        .run_with_session(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "user",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(result.execution_result.usage.total_tokens, Some(14));
    let (_, tool, error) = result
        .execution_result
        .messages
        .iter()
        .find_map(Message::tool_result)
        .unwrap();
    assert!(error);
    assert_eq!(tool["error"]["message"], "FINAL_TOOL_ERROR");
    assert_eq!(tool["error"]["code"], "delivery_error");
    assert_eq!(tool["interception"]["execution"], "succeeded");
}
#[tokio::test]
async fn deferred_custom_finalization_follows_candidate_checkpoint_without_extra_request() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        gate: true,
        ..Default::default()
    });
    let h = Arc::new(Hook::default());
    let a = agent(
        root.path(),
        p.clone(),
        h.clone(),
        Arc::new(AtomicUsize::new(0)),
    );
    let repo = Arc::new(MemorySessionRepository::new());
    let s = repo.create(root.path()).unwrap();
    let id = s.metadata().id.clone();
    let (handle, control) = a
        .control_channel(TurnExecutionContext::new(), RunControlConfig::default())
        .unwrap();
    let owned = a.clone();
    let repository = repo.clone();
    let task = tokio::spawn(async move {
        owned
            .run_prompt_with_session_controlled(
                repository.as_ref(),
                SessionSelection::Existing(id),
                "initial",
                Arc::new(()),
                control,
            )
            .await
    });
    p.started.notified().await;
    handle
        .submit_custom(
            CustomDelivery::TurnEnd,
            CustomMessage::text("sender", "note", "context").unwrap(),
        )
        .await
        .unwrap();
    p.release.notify_one();
    task.await.unwrap().into_result().unwrap();
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    let saved = repo.open(&s.metadata().id).unwrap();
    let messages = saved.messages();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1].message.role, MessageRole::Assistant);
    assert_eq!(messages[2].message.role, MessageRole::Custom);
    assert!(
        messages[2]
            .message
            .text_content()
            .contains("FINAL_CUSTOM:context")
    );
    assert_eq!(
        h.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.0 == MessageOrigin::Deferred)
            .count(),
        1
    );
}
struct LargeTool;
#[async_trait]
impl MessageHook for LargeTool {
    async fn finalize(
        &self,
        i: MessageHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<Message>> {
        if i.message.role != MessageRole::Tool {
            return Ok(None);
        }
        let mut message = i.message.clone();
        if let ContentBlock::ToolResult { content, .. } = &mut message.content[0] {
            content["output"] = json!("x".repeat(5000));
        }
        Ok(Some(message))
    }
}
#[tokio::test]
async fn final_tool_payload_cannot_exceed_the_owned_dispatcher_output_limit() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        tool: true,
        ..Default::default()
    });
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Effect(count.clone())).unwrap();
    let a = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .registry(tools)
            .dispatcher_config(DispatcherConfig {
                max_output_bytes: 4096,
                ..Default::default()
            })
            .extensions(extensions(Arc::new(LargeTool)))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let result = a
        .run_outcome("user", Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(!matches!(result, AgentTurnOutcome::Completed(_)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert!(result.progress().messages.iter().any(|m| {
        m.tool_result()
            .is_some_and(|(_, v, _)| v["output"]["raw"] == "RAW_TOOL")
    }));
}
struct SlowToolJournal(Mutex<Vec<Message>>);
#[async_trait]
impl RunJournal for SlowToolJournal {
    async fn record_message_finalization(&self, _: &MessageFinalizationRecord) -> Result<()> {
        Ok(())
    }
    async fn finalize_message(&self, c: &MessageFinalizationCommit) -> Result<()> {
        if c.finalized.message.role == MessageRole::Tool {
            std::thread::sleep(Duration::from_millis(250));
        }
        self.0.lock().unwrap().push(c.finalized.message.clone());
        Ok(())
    }
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn acknowledged_tool_finalization_survives_a_duration_expiring_during_commit() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        tool: true,
        ..Default::default()
    });
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Effect(count.clone())).unwrap();
    let executor = TurnExecutor::builder_from_shared(p.clone())
        .workspace(root.path())
        .registry(tools)
        .extensions(extensions(Arc::new(Hook::default())))
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_duration: Some(Duration::from_millis(200)),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let journal = SlowToolJournal(Mutex::new(Vec::new()));
    let result = executor
        .run_with_journal(
            ModelRequest::new("m", vec![Message::text(MessageRole::User, "history")]),
            Arc::new(()),
            TurnExecutionContext::new(),
            &journal,
        )
        .await;
    assert!(matches!(result, RunOutcome::Interrupted(_)));
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(result.progress().usage.total_tokens, Some(7));
    let persisted = journal.0.lock().unwrap().last().unwrap().clone();
    assert_eq!(result.progress().messages.last(), Some(&persisted));
    assert_eq!(
        persisted.tool_result().unwrap().1["output"]["final"],
        "FINAL_TOOL"
    );
}
