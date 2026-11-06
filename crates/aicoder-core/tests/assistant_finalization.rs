use aicoder_core::{events::*, extensions::*, model::*, session::*, *};
use anyhow::Result;
use async_trait::async_trait;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[derive(Default)]
struct Provider {
    calls: AtomicUsize,
    tools: bool,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", m);
        s.capabilities.tools = true;
        s.context_tokens = Some(16384);
        Ok(s)
    }
    async fn generate(
        &self,
        r: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = ModelResponse {
            id: None,
            model: r.model,
            message: if self.tools {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "call".into(),
                            name: "hidden".into(),
                            arguments: serde_json::json!({"owned":true}),
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::Assistant, "RAW_MODEL_TEXT")
            },
            stop_reason: if self.tools {
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
struct Edit {
    mode: u8,
    seen: Arc<AtomicUsize>,
}
#[async_trait]
impl AssistantMessageHook for Edit {
    async fn finalize(
        &self,
        input: AssistantMessageInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<Message>> {
        self.seen.fetch_add(1, Ordering::SeqCst);
        assert_eq!(input.usage.total_tokens, Some(7));
        Ok(Some(match self.mode {
            0 => Message::text(MessageRole::Assistant, "FINAL_TEXT"),
            1 => {
                assert_eq!(input.message.text_content(), "FINAL_TEXT");
                Message::text(MessageRole::Assistant, "CHAINED_FINAL")
            }
            2 => Message::text(MessageRole::User, "wrong role"),
            3 => Message::text(MessageRole::Assistant, "x".repeat(MAX_FINAL_MESSAGE_BYTES)),
            4 => anyhow::bail!("PRIVATE_NATIVE_FAILURE"),
            5 => panic!("PRIVATE_NATIVE_PANIC"),
            _ => unreachable!(),
        }))
    }
}
fn extensions(h: Arc<dyn AssistantMessageHook>, timeout: Option<Duration>) -> Arc<ExtensionSet> {
    let mut e = Extension::new("finalize", "1").unwrap();
    let mut spec = HookSpec::new("edit");
    if let Some(timeout) = timeout {
        spec.timeout = timeout;
    }
    e.assistant_message_hook(spec, h).unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    Arc::new(r.finish())
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, e: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(e.clone());
    }
}
#[tokio::test]
async fn ordered_finalization_is_canonical_once_and_keeps_paid_usage_and_raw_stream_distinct() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider::default());
    let seen = Arc::new(AtomicUsize::new(0));
    let mut e = Extension::new("chain", "1").unwrap();
    e.assistant_message_hook(
        HookSpec::new("first"),
        Arc::new(Edit {
            mode: 0,
            seen: seen.clone(),
        }),
    )
    .unwrap();
    let mut late = HookSpec::new("last");
    late.priority = 10;
    e.assistant_message_hook(
        late,
        Arc::new(Edit {
            mode: 1,
            seen: seen.clone(),
        }),
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
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let events = Arc::new(Events::default());
    let result = agent
        .run_with_session(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "user",
            events.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.execution_result.final_message.text_content(),
        "CHAINED_FINAL"
    );
    assert_eq!(result.execution_result.usage.total_tokens, Some(7));
    assert_eq!(seen.load(Ordering::SeqCst), 2);
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    let saved = repo.open(&session.metadata().id).unwrap();
    assert_eq!(saved.messages().len(), 2);
    assert_eq!(saved.messages()[1].message.text_content(), "CHAINED_FINAL");
    assert!(
        !serde_json::to_string(saved.entries())
            .unwrap()
            .contains("RAW_MODEL_TEXT")
    );
    let raw = events.0.lock().unwrap();
    assert!(raw.iter().any(|e|matches!(&e.event,AgentRawEvent::ContentChunk{delta,..}if delta.as_ref()=="RAW_MODEL_TEXT")));
    assert!(raw.iter().any(|e|matches!(&e.event,AgentRawEvent::AssistantMessageFinalized{message,changed:true}if message.text_content()=="CHAINED_FINAL")));
    let mut validator = EventTraceValidator::new();
    for e in raw.iter() {
        validator.observe(e).unwrap();
    }
    validator.finish().unwrap();
}
#[tokio::test]
async fn failed_role_size_error_and_panic_edits_keep_completed_response_and_never_repeat_model() {
    for mode in [2, 3, 4, 5] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let executor = TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(
                Arc::new(Edit {
                    mode,
                    seen: Arc::new(AtomicUsize::new(0)),
                }),
                None,
            ))
            .build()
            .unwrap();
        let result = executor
            .run_outcome(
                ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(matches!(result, RunOutcome::Interrupted(_)));
        assert_eq!(result.progress().usage.total_tokens, Some(7));
        assert_eq!(
            result.progress().messages.last().unwrap().text_content(),
            "RAW_MODEL_TEXT"
        );
        assert_eq!(p.calls.load(Ordering::SeqCst), 1);
        assert!(!format!("{result:?}").contains("PRIVATE_NATIVE_FAILURE"));
        assert!(!format!("{result:?}").contains("PRIVATE_NATIVE_PANIC"));
    }
}
#[tokio::test]
async fn finalizers_cannot_erase_or_rewrite_tool_actions_before_owned_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let p = Arc::new(Provider {
        tools: true,
        ..Default::default()
    });
    let executor = TurnExecutor::builder_from_shared(p.clone())
        .workspace(root.path())
        .extensions(extensions(
            Arc::new(Edit {
                mode: 0,
                seen: Arc::new(AtomicUsize::new(0)),
            }),
            None,
        ))
        .build()
        .unwrap();
    let outcome = executor
        .run_outcome(
            ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        outcome
            .progress()
            .messages
            .last()
            .unwrap()
            .tool_calls()
            .count(),
        1
    );
    assert_eq!(outcome.progress().tool_calls, 0);
}
struct Wait {
    started: Arc<tokio::sync::Notify>,
    ended: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl AssistantMessageHook for Wait {
    async fn finalize(
        &self,
        _: AssistantMessageInput<'_>,
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
async fn cancellation_and_timeout_drop_private_children_and_preserve_paid_completion() {
    for cancel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let started = Arc::new(tokio::sync::Notify::new());
        let ended = Arc::new(tokio::sync::Notify::new());
        let executor = TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(
                Arc::new(Wait {
                    started: started.clone(),
                    ended: ended.clone(),
                }),
                Some(Duration::from_millis(30)),
            ))
            .build()
            .unwrap();
        let context = TurnExecutionContext::new();
        let worker = context.clone();
        let task = tokio::spawn(async move {
            executor
                .run_outcome(
                    ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
                    Arc::new(()),
                    worker,
                )
                .await
        });
        started.notified().await;
        if cancel {
            context.cancel("cancel finalization");
        }
        let outcome = task.await.unwrap();
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(outcome.progress().usage.total_tokens, Some(7));
        assert_eq!(
            outcome.progress().messages.last().unwrap().text_content(),
            "RAW_MODEL_TEXT"
        );
        tokio::time::timeout(Duration::from_secs(1), ended.notified())
            .await
            .unwrap();
        assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    }
}
struct Journal {
    supported: bool,
    fail: bool,
    slow: bool,
}
#[async_trait]
impl RunJournal for Journal {
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn record_finalization(&self, _: &FinalizationRecord) -> Result<()> {
        anyhow::ensure!(self.supported, "unsupported");
        Ok(())
    }
    async fn finalize_assistant(
        &self,
        _: usize,
        _: &FinalizationRecord,
        _: &Message,
    ) -> Result<()> {
        anyhow::ensure!(!self.fail, "commit refused");
        if self.slow {
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn unsupported_failed_and_slow_journals_never_retry_and_apply_only_acknowledged_final_text() {
    for (supported, fail, slow) in [
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let executor = TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(
                Arc::new(Edit {
                    mode: 0,
                    seen: Arc::new(AtomicUsize::new(0)),
                }),
                None,
            ))
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_duration: Some(Duration::from_millis(200)),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap();
        let outcome = executor
            .run_with_journal(
                ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
                Arc::new(()),
                TurnExecutionContext::new(),
                &Journal {
                    supported,
                    fail,
                    slow,
                },
            )
            .await;
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(p.calls.load(Ordering::SeqCst), usize::from(supported));
        if supported {
            assert_eq!(outcome.progress().usage.total_tokens, Some(7));
            assert_eq!(
                outcome.progress().messages.last().unwrap().text_content(),
                if slow { "FINAL_TEXT" } else { "RAW_MODEL_TEXT" }
            );
        }
    }
}
struct Verify;
#[async_trait]
impl CompletionVerifier for Verify {
    async fn verify(&self, c: CompletionContext<'_>) -> Result<CompletionVerdict> {
        anyhow::ensure!(
            c.candidate.text_content() == "FINAL_TEXT",
            "verifier saw preview"
        );
        Ok(CompletionVerdict::Accepted)
    }
}
#[tokio::test]
async fn verifier_and_budget_admission_use_whole_finalized_message() {
    for limited in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider::default());
        let executor = TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(extensions(
                Arc::new(Edit {
                    mode: 0,
                    seen: Arc::new(AtomicUsize::new(0)),
                }),
                None,
            ))
            .completion_verifier(Verify)
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_output_bytes: limited.then_some(40),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap();
        let outcome = executor
            .run_outcome(
                ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert_eq!(matches!(outcome, RunOutcome::Completed(_)), !limited);
        assert_eq!(p.calls.load(Ordering::SeqCst), 1);
        assert_eq!(outcome.progress().usage.total_tokens, Some(7));
    }
}
