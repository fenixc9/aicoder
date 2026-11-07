use aicoder_core::{extensions::*, model::*, session::*, *};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;
fn usage(n: u64) -> ModelUsage {
    ModelUsage {
        input_tokens: Some(n - 1),
        output_tokens: Some(1),
        total_tokens: Some(n),
        cached_input_tokens: Some(0),
        cache_write_tokens: Some(0),
    }
}
#[derive(Clone, Copy)]
enum Mode {
    Main,
    Aux,
    Fail,
    Wait,
    Unknown,
    Large,
    Long,
    UnexpectedTool,
}
struct Provider {
    mode: Mode,
    requests: Mutex<Vec<ModelRequest>>,
    started: Notify,
    dropped: AtomicBool,
}
impl Provider {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            requests: Mutex::new(vec![]),
            started: Notify::new(),
            dropped: AtomicBool::new(false),
        })
    }
    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
struct Dropped<'a>(&'a AtomicBool);
impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new(
            if matches!(self.mode, Mode::Main) {
                "main"
            } else {
                "native"
            },
            m,
        );
        s.context_tokens = Some(65536);
        s.max_output_tokens = Some(128);
        Ok(s)
    }
    async fn generate(
        &self,
        r: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let n = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(r.clone());
            requests.len()
        };
        if matches!(self.mode, Mode::Wait | Mode::Fail) {
            sink.emit(ModelEvent::Usage { usage: usage(2) });
        }
        if matches!(self.mode, Mode::Fail) {
            return Err(ProviderError::new(
                "native",
                ProviderErrorKind::Other,
                "PRIVATE_PROVIDER_FAILURE",
            ));
        }
        if matches!(self.mode, Mode::Wait) {
            let _guard = Dropped(&self.dropped);
            self.started.notify_one();
            return std::future::pending().await;
        }
        let text = match self.mode {
            Mode::Main => format!("answer-{n}"),
            Mode::Large => "x".repeat(300 * 1024),
            Mode::Long => "x".repeat(500),
            _ => "native-summary".into(),
        };
        let response = ModelResponse {
            id: None,
            model: r.model,
            message: if matches!(self.mode, Mode::UnexpectedTool) {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "unexpected".into(),
                            name: "write_file".into(),
                            arguments: json!({}),
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::Assistant, text)
            },
            stop_reason: if matches!(self.mode, Mode::UnexpectedTool) {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: if matches!(self.mode, Mode::Unknown) {
                ModelUsage::default()
            } else {
                usage(if matches!(self.mode, Mode::Main) {
                    7
                } else {
                    3
                })
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
enum Action {
    Custom,
    EditCompact(Arc<Mutex<Option<String>>>),
    Null,
    Invalid,
    Image,
    Oversized,
    Work(Arc<Provider>, bool),
    Replace,
}
#[async_trait]
impl BoundaryHook for Action {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        if input.kind != BoundaryKind::TurnEnd || input.round != 1 {
            return Ok(None);
        }
        let mut action = BoundaryAction::default();
        match self {
            Self::Custom => {
                action.append.push(BoundaryEntry::CustomMessage {
                    kind: "note".into(),
                    content: vec![ContentBlock::Text {
                        text: "staged-original".into(),
                    }],
                    display: false,
                    details: Some(json!({"grant":true,"private":"INERT_DETAILS"})),
                });
                action.continue_run = Some(true);
            }
            Self::EditCompact(saved) => {
                let id = input.entries[0].entry_id.clone().unwrap();
                *saved.lock().unwrap() = Some(id.clone());
                let session = input.session_context.unwrap();
                assert!(session.entry_ids.iter().any(|v| v.as_ref() == Some(&id)));
                assert!(
                    input
                        .context_preview
                        .iter()
                        .any(|m| m.text_content().contains("staged-original"))
                );
                assert!(
                    !input
                        .history
                        .iter()
                        .any(|m| m.text_content().contains("staged-original"))
                );
                action.append = vec![
                    BoundaryEntry::ContextEdit {
                        target_id: id.clone(),
                        replacement: ContextReplacement::Text("edited-stage".into()),
                    },
                    BoundaryEntry::Compaction {
                        summary: "retained host summary".into(),
                        first_kept_entry_id: Some(id),
                        details: Some(json!({"instructions":"INERT_COMPACTION_DETAILS"})),
                    },
                ];
            }
            Self::Null => {
                action.append.push(BoundaryEntry::Compaction {
                    summary: "self-retaining summary".into(),
                    first_kept_entry_id: None,
                    details: None,
                });
                action.continue_run = Some(true);
            }
            Self::Invalid => {
                action.append = vec![
                    BoundaryEntry::CustomMessage {
                        kind: "note".into(),
                        content: vec![ContentBlock::Text {
                            text: "must not publish".into(),
                        }],
                        display: true,
                        details: None,
                    },
                    BoundaryEntry::ContextEdit {
                        target_id: "missing".into(),
                        replacement: ContextReplacement::Omit,
                    },
                ];
                action.continue_run = Some(true);
            }
            Self::Image => action.append.push(BoundaryEntry::CustomMessage {
                kind: "image".into(),
                content: vec![ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://example.invalid/image.png".into(),
                    },
                }],
                display: true,
                details: None,
            }),
            Self::Oversized => action.append.push(BoundaryEntry::Compaction {
                summary: "x".repeat(4096),
                first_kept_entry_id: None,
                details: None,
            }),
            Self::Work(provider, fail) => {
                let mut request = ModelRequest::new(
                    "aux",
                    vec![
                        Message::text(
                            MessageRole::System,
                            "Summarize the supplied historical data",
                        ),
                        Message::text(MessageRole::User, "facts"),
                    ],
                );
                request.tool_choice = ModelToolChoice::None;
                request.options.max_output_tokens = Some(64);
                let response = input.models.generate(provider.clone(), request).await?;
                ensure!(!fail, "PRIVATE_CALLBACK_FAILURE");
                action.append.push(BoundaryEntry::Compaction {
                    summary: response.message.text_content(),
                    first_kept_entry_id: None,
                    details: None,
                });
                action.continue_run = Some(true);
            }
            Self::Replace => {
                assert_eq!(input.statistics.usage.total_tokens, Some(10));
                assert_eq!(
                    input
                        .statistics
                        .primary_usage
                        .as_ref()
                        .unwrap()
                        .total_tokens,
                    Some(7)
                );
                assert_eq!(input.statistics.auxiliary.len(), 1);
                action.replace = Some(vec![]);
                action.continue_run = Some(false);
            }
        }
        Ok(Some(action))
    }
}
fn agent(
    root: &std::path::Path,
    p: Arc<dyn ModelProvider>,
    actions: Vec<Action>,
    budget: RunBudget,
) -> Agent {
    let mut extension = Extension::new("drafts", "1").unwrap();
    for (i, a) in actions.into_iter().enumerate() {
        let mut spec = HookSpec::new(format!("h{i}"));
        spec.priority = i as i32;
        extension.boundary_hook(spec, Arc::new(a)).unwrap();
    }
    let mut registry = ExtensionRegistry::new();
    registry.install(extension).unwrap();
    Agent::new(
        TurnExecutor::builder_from_shared(p)
            .workspace(root)
            .extensions(Arc::new(registry.finish()))
            .config(TurnExecutionConfig {
                budget,
                ..Default::default()
            })
            .build()
            .unwrap(),
        AgentConfig::new("main").system_prompt("ROOT_PREFIX_SENTINEL"),
    )
}
fn source_records(s: &Session, namespace: &str) -> usize {
    s.entries()
        .iter()
        .filter(|e| matches!(&e.data,SessionEntryData::Extension {namespace:n,..}if n==namespace))
        .count()
}
#[tokio::test]
async fn later_callbacks_edit_fixed_pending_ids_and_atomically_compact_before_continuing() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = repo.create(root.path()).unwrap();
    let p = Provider::new(Mode::Main);
    let saved = Arc::new(Mutex::new(None));
    let a = agent(
        root.path(),
        p.clone(),
        vec![Action::Custom, Action::EditCompact(saved.clone())],
        RunBudget::default(),
    );
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    assert_eq!(p.calls(), 2);
    assert_eq!(out.progress().usage.total_tokens, Some(14));
    let session = repo.open(&s.metadata().id).unwrap();
    let id = saved.lock().unwrap().clone().unwrap();
    assert!(matches!(
        session.entry(&id).unwrap().data,
        SessionEntryData::CustomMessage { .. }
    ));
    assert!(
        session
            .entry(&id)
            .unwrap()
            .data
            .as_message()
            .unwrap()
            .text_content()
            .contains("staged-original")
    );
    let requests = p.requests.lock().unwrap();
    let next = &requests[1];
    assert_eq!(next.messages[0].text_content(), "ROOT_PREFIX_SENTINEL");
    assert!(
        next.messages
            .iter()
            .any(|m| m.text_content().contains("edited-stage"))
    );
    assert!(
        !next
            .messages
            .iter()
            .any(|m| m.text_content() == "task" || m.text_content() == "answer-1")
    );
    assert!(!serde_json::to_string(next).unwrap().contains("INERT_"));
    assert_eq!(
        source_records(&session, summary::NATIVE_COMPACTION_NAMESPACE),
        1
    );
    assert_eq!(source_records(&session, CONTEXT_EDIT_NAMESPACE), 1);
    let view = session.selected_branch().model_context().unwrap().messages;
    assert_eq!(view.last().unwrap().text_content(), "answer-2");
    assert_eq!(session.messages().len(), 4);
    assert_eq!(
        out.progress().model_context.as_ref().unwrap().compactions,
        1
    );
}
#[tokio::test]
async fn self_retaining_reduction_keeps_future_records_and_original_raw_history() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = repo.create(root.path()).unwrap();
    let p = Provider::new(Mode::Main);
    let a = agent(
        root.path(),
        p.clone(),
        vec![Action::Null],
        RunBudget::default(),
    );
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    let mut saved = repo.open(&s.metadata().id).unwrap();
    assert_eq!(saved.messages().len(), 3);
    let view = saved.selected_branch().model_context().unwrap().messages;
    assert_eq!(view.len(), 2);
    assert!(view[0].text_content().contains("self-retaining"));
    assert_eq!(view[1].text_content(), "answer-2");
    repo.append(&mut saved, Message::text(MessageRole::User, "future fact"))
        .unwrap();
    assert_eq!(
        saved
            .selected_branch()
            .model_context()
            .unwrap()
            .messages
            .last()
            .unwrap()
            .text_content(),
        "future fact"
    );
    assert_eq!(saved.messages().len(), 4);
}
#[tokio::test]
async fn invalid_or_incompatible_plans_publish_no_partial_entries_after_paid_response() {
    for action in [Action::Invalid, Action::Image] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let s = repo.create(root.path()).unwrap();
        let p = Provider::new(Mode::Main);
        let a = agent(root.path(), p.clone(), vec![action], RunBudget::default());
        let out = a
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(s.metadata().id.clone()),
                "task",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(!matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
        assert_eq!(p.calls(), 1);
        assert_eq!(out.progress().usage.total_tokens, Some(7));
        let saved = repo.open(&s.metadata().id).unwrap();
        assert_eq!(saved.messages().len(), 2);
        assert!(
            !saved
                .entries()
                .iter()
                .any(|e| matches!(e.data, SessionEntryData::CustomMessage { .. }))
        );
        assert_eq!(
            source_records(&saved, summary::NATIVE_COMPACTION_NAMESPACE),
            0
        );
    }
}
#[tokio::test]
async fn native_usage_is_charged_when_a_later_callback_replaces_the_draft() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = repo.create(root.path()).unwrap();
    let p = Provider::new(Mode::Main);
    let aux = Provider::new(Mode::Aux);
    let a = agent(
        root.path(),
        p.clone(),
        vec![Action::Work(aux.clone(), false), Action::Replace],
        RunBudget::default(),
    );
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    assert_eq!(p.calls(), 1);
    assert_eq!(aux.calls(), 1);
    assert_eq!(out.progress().usage.total_tokens, Some(10));
    assert_eq!(
        out.progress().primary_usage.as_ref().unwrap().total_tokens,
        Some(7)
    );
    assert_eq!(out.progress().auxiliary.len(), 1);
    let saved = repo.open(&s.metadata().id).unwrap();
    assert_eq!(
        source_records(&saved, summary::NATIVE_COMPACTION_NAMESPACE),
        0
    );
    assert_eq!(source_records(&saved, auxiliary::AUXILIARY_NAMESPACE), 2);
}
#[tokio::test]
async fn callback_failure_and_invalid_delivery_keep_completed_native_usage_without_retry() {
    for (mode, callback_failure) in [
        (Mode::Aux, true),
        (Mode::Fail, false),
        (Mode::Large, false),
        (Mode::UnexpectedTool, false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let s = repo.create(root.path()).unwrap();
        let p = Provider::new(Mode::Main);
        let aux = Provider::new(mode);
        let a = agent(
            root.path(),
            p.clone(),
            vec![Action::Work(aux.clone(), callback_failure)],
            RunBudget::default(),
        );
        let out = a
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(s.metadata().id.clone()),
                "task",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(!matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
        assert_eq!(p.calls(), 1);
        assert_eq!(aux.calls(), 1);
        assert_eq!(
            out.progress().usage.total_tokens,
            Some(if matches!(mode, Mode::Fail) { 9 } else { 10 })
        );
        assert_eq!(out.progress().auxiliary.len(), 1);
        assert!(!format!("{out:?}").contains("PRIVATE_"));
        assert_eq!(
            source_records(
                &repo.open(&s.metadata().id).unwrap(),
                summary::NATIVE_COMPACTION_NAMESPACE
            ),
            0
        );
        if matches!(mode, Mode::Large | Mode::UnexpectedTool) {
            assert!(!out.progress().auxiliary[0].delivery_complete);
            assert_eq!(out.progress().auxiliary[0].status, "completed");
        }
    }
}
#[tokio::test]
async fn cancelled_callback_drops_native_owner_and_retains_known_partial_usage() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = repo.create(root.path()).unwrap();
    let p = Provider::new(Mode::Main);
    let aux = Provider::new(Mode::Wait);
    let a = agent(
        root.path(),
        p.clone(),
        vec![Action::Work(aux.clone(), false)],
        RunBudget::default(),
    );
    let context = TurnExecutionContext::new();
    let future = a.run_with_session_outcome(
        &repo,
        SessionSelection::Existing(s.metadata().id.clone()),
        "task",
        Arc::new(()),
        context.clone(),
    );
    tokio::pin!(future);
    tokio::select! {out=&mut future=>panic!("completed before cancellation: {out:?}"),_=aux.started.notified()=>context.cancel("stop")};
    let out = future.await;
    assert!(!matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    assert!(aux.dropped.load(Ordering::SeqCst));
    assert_eq!(p.calls(), 1);
    assert_eq!(out.progress().usage.total_tokens, Some(9));
    assert_eq!(out.progress().auxiliary[0].status, "interrupted");
    assert!(!out.progress().auxiliary[0].delivery_complete);
}
#[tokio::test]
async fn native_reservation_checks_parent_limits_before_provider_io() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = repo.create(root.path()).unwrap();
    let p = Provider::new(Mode::Main);
    let aux = Provider::new(Mode::Aux);
    let a = agent(
        root.path(),
        p.clone(),
        vec![Action::Work(aux.clone(), false)],
        RunBudget {
            max_auxiliary_calls: Some(0),
            ..Default::default()
        },
    );
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(!matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    assert_eq!(p.calls(), 1);
    assert_eq!(aux.calls(), 0);
    assert_eq!(out.progress().usage.total_tokens, Some(7));
    assert!(out.progress().auxiliary.is_empty());
}
#[tokio::test]
async fn ephemeral_runs_support_self_retaining_drafts_and_unknown_usage_is_never_free() {
    let root = tempfile::tempdir().unwrap();
    let p = Provider::new(Mode::Main);
    let aux = Provider::new(Mode::Unknown);
    let a = agent(
        root.path(),
        p.clone(),
        vec![Action::Work(aux.clone(), false)],
        RunBudget::default(),
    );
    let out = a
        .run_outcome("task", Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    assert_eq!(p.calls(), 2);
    assert_eq!(aux.calls(), 1);
    assert_eq!(out.progress().usage.total_tokens, None);
    assert_eq!(
        out.progress().primary_usage.as_ref().unwrap().total_tokens,
        Some(14)
    );
    assert!(out.progress().tokens.estimated);
    assert!(out.progress().tokens.used > 14);
    assert!(!out.progress().auxiliary[0].reported_usage);
    assert!(
        p.requests.lock().unwrap()[1]
            .messages
            .iter()
            .any(|m| m.text_content().contains("native-summary"))
    );
    assert_eq!(out.progress().messages.len(), 4);
}

#[tokio::test]
async fn continuation_after_native_work_keeps_primary_usage_when_the_budget_clamps_output() {
    let root = tempfile::tempdir().unwrap();
    let p = Provider::new(Mode::Main);
    let aux = Provider::new(Mode::Long);
    let summary = summary::NativeCompactionRecord {
        version: 1,
        extension: "drafts".into(),
        hook: "h0".into(),
        source_leaf: None,
        source_revision: 0,
        first_kept_entry_id: None,
        replaced_messages: 0,
        retained_messages: 0,
        summary: "x".repeat(500),
        details: None,
        uncertain_operations: false,
    };
    let q = ModelRequest::new(
        "main",
        vec![
            Message::text(MessageRole::System, "ROOT_PREFIX_SENTINEL"),
            summary.message(),
        ],
    );
    let count = tokens::TokenCounting::default()
        .request(&q, &p.model_spec("main").unwrap())
        .tokens;
    let a = agent(
        root.path(),
        p.clone(),
        vec![Action::Work(aux, false)],
        RunBudget {
            max_tokens: Some(count + 10 + 32),
            ..Default::default()
        },
    );
    let out = a
        .run_outcome("task", Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    assert_eq!(p.calls(), 2);
    assert!(
        p.requests.lock().unwrap()[1]
            .options
            .max_output_tokens
            .unwrap()
            < 128
    );
    assert_eq!(
        out.progress().primary_usage.as_ref().unwrap().total_tokens,
        Some(14)
    );
    assert_eq!(out.progress().usage.total_tokens, Some(17));
    assert_eq!(out.progress().tokens.used, 17);
}
struct FinalizeCustom;
#[async_trait]
impl MessageHook for FinalizeCustom {
    async fn finalize(
        &self,
        i: MessageHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<Message>> {
        if i.origin != MessageOrigin::Boundary || i.message.role != MessageRole::Custom {
            return Ok(None);
        }
        assert!(!i.custom.unwrap().display);
        let mut m = i.message.clone();
        m.content[1] = ContentBlock::Text {
            text: "finalized custom".into(),
        };
        Ok(Some(m))
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, e: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(e.clone());
    }
}
#[tokio::test]
async fn boundary_custom_messages_are_finalized_once_with_source_and_hidden_display_preserved() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = repo.create(root.path()).unwrap();
    let p = Provider::new(Mode::Main);
    let mut e = Extension::new("drafts", "1").unwrap();
    e.boundary_hook(HookSpec::new("h0"), Arc::new(Action::Custom))
        .unwrap();
    e.message_hook(HookSpec::new("fresh"), Arc::new(FinalizeCustom))
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let a = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("main"),
    );
    let events = Arc::new(Events::default());
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            events.clone(),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    let saved = repo.open(&s.metadata().id).unwrap();
    let custom = saved
        .entries()
        .iter()
        .find_map(|e| {
            if let SessionEntryData::CustomMessage { custom } = &e.data {
                Some(custom)
            } else {
                None
            }
        })
        .unwrap();
    assert!(!custom.display());
    assert!(custom.message().text_content().contains("finalized custom"));
    assert_eq!(custom.source(), "drafts");
    assert!(
        p.requests.lock().unwrap()[1]
            .messages
            .iter()
            .any(|m| m.text_content().contains("finalized custom"))
    );
    assert!(events.0.lock().unwrap().iter().any(|e|matches!(&e.event,AgentRawEvent::MessageFinalized {origin:MessageOrigin::Boundary,custom:Some(c),..} if !c.display)));
}
#[tokio::test]
async fn native_callback_deadline_drops_work_and_keeps_partial_paid_usage() {
    let root = tempfile::tempdir().unwrap();
    let p = Provider::new(Mode::Main);
    let aux = Provider::new(Mode::Wait);
    let a = agent(
        root.path(),
        p.clone(),
        vec![Action::Work(aux.clone(), false)],
        RunBudget {
            max_duration: Some(std::time::Duration::from_millis(50)),
            ..Default::default()
        },
    );
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        a.run_outcome("task", Arc::new(()), TurnExecutionContext::new()),
    )
    .await
    .unwrap();
    assert!(!matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    assert_eq!(p.calls(), 1);
    assert_eq!(aux.calls(), 1);
    assert!(aux.dropped.load(Ordering::SeqCst));
    assert_eq!(out.progress().usage.total_tokens, Some(9));
    assert_eq!(out.progress().auxiliary[0].status, "interrupted");
}

#[tokio::test]
async fn finished_native_receipts_remain_known_usage_without_terminal_turn_statistics() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = repo.create(root.path()).unwrap();
    let p = Provider::new(Mode::Main);
    let aux = Provider::new(Mode::Aux);
    let a = agent(
        root.path(),
        p,
        vec![Action::Work(aux, false), Action::Replace],
        RunBudget::default(),
    );
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    let full = repo.open(&s.metadata().id).unwrap();
    assert_eq!(
        full.selected_branch()
            .accounting()
            .unwrap()
            .usage
            .total_tokens,
        Some(10)
    );
    let mut records = full.entries().to_vec();
    assert!(matches!(
        records.pop().unwrap().data,
        SessionEntryData::TurnEnd { .. }
    ));
    let mut metadata = full.metadata().clone();
    metadata.leaf_id = records.last().map(|e| e.id.clone());
    let partial = Session::from_entries(metadata, records).unwrap();
    assert_eq!(
        partial
            .selected_branch()
            .accounting()
            .unwrap()
            .usage
            .total_tokens,
        Some(3)
    );
    assert_eq!(
        partial.selected_branch().accounting().unwrap().tokens.used,
        3
    );
}

struct Small(Arc<Provider>);
#[async_trait]
impl ModelProvider for Small {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = self.0.model_spec(m)?;
        s.context_tokens = Some(512);
        Ok(s)
    }
    async fn generate(
        &self,
        r: ModelRequest,
        e: Arc<dyn ModelEventSink>,
        c: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.0.generate(r, e, c).await
    }
}
#[tokio::test]
async fn complete_projected_window_is_admitted_before_any_boundary_record_is_written() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = repo.create(root.path()).unwrap();
    let p = Provider::new(Mode::Main);
    let a = agent(
        root.path(),
        Arc::new(Small(p.clone())),
        vec![Action::Oversized],
        RunBudget::default(),
    );
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(!matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    assert_eq!(p.calls(), 1);
    assert_eq!(out.progress().usage.total_tokens, Some(7));
    assert_eq!(
        source_records(
            &repo.open(&s.metadata().id).unwrap(),
            summary::NATIVE_COMPACTION_NAMESPACE
        ),
        0
    );
}
