use aicoder_core::{
    context::*, events::*, extensions::*, model::*, session::*, summary::*, tools::*, *,
};
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
    tools: bool,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", model);
        s.capabilities.tools = true;
        s.context_tokens = Some(16384);
        s.max_output_tokens = Some(1024);
        Ok(s)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let summary = request
            .messages
            .first()
            .is_some_and(|m| m.text_content().starts_with("Summarize historical"));
        let tools = self.tools
            && !summary
            && request
                .messages
                .last()
                .is_some_and(|m| m.role == MessageRole::User);
        let n = self.requests.lock().unwrap().len() + 1;
        self.requests.lock().unwrap().push(request.clone());
        let message = if summary {
            Message::text(MessageRole::Assistant,json!({"goals":[],"constraints":[],"changes":[],"todos":[],"notes":["context retained"]}).to_string())
        } else if tools {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "echo".into(),
                        name: "echo".into(),
                        arguments: json!({}),
                    },
                }],
            }
        } else {
            Message::text(MessageRole::Assistant, format!("candidate-{n}"))
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if tools {
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
fn model() -> Arc<Provider> {
    Arc::new(Provider::default())
}
fn set(hook: Arc<dyn BoundaryHook>) -> Arc<ExtensionSet> {
    let mut e = Extension::new("boundary", "1").unwrap();
    e.boundary_hook(HookSpec::new("action"), hook).unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    Arc::new(r.finish())
}
struct Once {
    kind: BoundaryKind,
    again: bool,
    calls: Arc<Mutex<Vec<BoundaryKind>>>,
}
#[async_trait]
impl BoundaryHook for Once {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        self.calls.lock().unwrap().push(input.kind);
        if input.kind != self.kind || input.round != 1 {
            return Ok(None);
        }
        assert_eq!(input.statistics.usage.total_tokens, Some(7));
        assert_eq!(input.statistics.rounds, 1);
        Ok(Some(BoundaryAction {
            append: vec![
                BoundaryEntry::Data {
                    key: "private-data".into(),
                    value: json!({"instructions":"INERT_DATA_NOT_PROMPTED","grant":true}),
                },
                BoundaryEntry::Context {
                    kind: "note".into(),
                    text: "EXPLICIT_CONTEXT_FACT".into(),
                },
            ],
            continue_run: Some(self.again),
            ..Default::default()
        }))
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(event.clone());
    }
}
#[tokio::test]
async fn round_and_settle_actions_commit_sourced_context_then_continue_the_same_budgeted_loop() {
    for kind in [BoundaryKind::TurnEnd, BoundaryKind::BeforeSettle] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let provider = model();
        let calls = Arc::new(Mutex::new(vec![]));
        let events = Arc::new(Events::default());
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(provider.clone())
                .workspace(root.path())
                .extensions(set(Arc::new(Once {
                    kind,
                    again: true,
                    calls: calls.clone(),
                })))
                .build()
                .unwrap(),
            AgentConfig::new("m"),
        );
        let turn = agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "user",
                events.clone(),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
        assert_eq!(turn.execution_result.rounds, 2);
        assert_eq!(turn.execution_result.usage.total_tokens, Some(14));
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]
                .messages
                .iter()
                .any(|m| m.role == MessageRole::System
                    && m.text_content().contains("EXPLICIT_CONTEXT_FACT"))
        );
        assert!(
            !serde_json::to_string(&*requests)
                .unwrap()
                .contains("INERT_DATA_NOT_PROMPTED")
        );
        let saved = repo.open(&session.metadata().id).unwrap();
        let messages = saved.messages();
        assert_eq!(
            messages
                .iter()
                .filter(|m| m.message.role == MessageRole::User)
                .count(),
            1
        );
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].message.text_content(), "candidate-1");
        assert!(
            messages[2]
                .message
                .text_content()
                .starts_with("[aicoder extension boundary/action/note]")
        );
        assert_eq!(messages[3].message.text_content(), "candidate-2");
        assert!(
            serde_json::to_string(saved.entries())
                .unwrap()
                .contains("INERT_DATA_NOT_PROMPTED")
        );
        let events = events.0.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e.event,
                    AgentRawEvent::LifecycleBoundaryCommitted {
                        continue_run: true,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(
            !serde_json::to_string(&*events)
                .unwrap()
                .contains("INERT_DATA_NOT_PROMPTED")
        );
    }
}
#[tokio::test]
async fn final_notes_without_continuation_do_not_call_the_model_or_replace_its_final_answer() {
    let root = tempfile::tempdir().unwrap();
    let provider = model();
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(set(Arc::new(Once {
            kind: BoundaryKind::BeforeSettle,
            again: false,
            calls: Arc::new(Mutex::new(vec![])),
        })))
        .build()
        .unwrap();
    let result = executor
        .run(ModelRequest::new(
            "m",
            vec![Message::text(MessageRole::User, "user")],
        ))
        .await
        .unwrap();
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert_eq!(result.final_message.text_content(), "candidate-1");
    assert!(
        result
            .messages
            .last()
            .unwrap()
            .text_content()
            .contains("EXPLICIT_CONTEXT_FACT")
    );
}
struct First;
#[async_trait]
impl BoundaryHook for First {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        if input.kind != BoundaryKind::BeforeSettle {
            return Ok(None);
        }
        Ok(Some(BoundaryAction {
            append: vec![BoundaryEntry::Context {
                kind: "old".into(),
                text: "REMOVED_PROPOSAL".into(),
            }],
            continue_run: Some(true),
            ..Default::default()
        }))
    }
}
struct Last;
#[async_trait]
impl BoundaryHook for Last {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        if input.kind != BoundaryKind::BeforeSettle {
            return Ok(None);
        }
        assert_eq!(input.entries.len(), 1);
        assert!(input.continue_requested);
        Ok(Some(BoundaryAction {
            replace: Some(vec![BoundaryEntry::Data {
                key: "only-data".into(),
                value: json!({"note":"recorded"}),
            }]),
            continue_run: Some(false),
            ..Default::default()
        }))
    }
}
#[tokio::test]
async fn later_handlers_see_and_replace_proposals_and_can_clear_requested_continuation() {
    let root = tempfile::tempdir().unwrap();
    let provider = model();
    let mut e = Extension::new("ordered", "1").unwrap();
    e.boundary_hook(
        HookSpec {
            priority: 10,
            ..HookSpec::new("last")
        },
        Arc::new(Last),
    )
    .unwrap();
    e.boundary_hook(
        HookSpec {
            priority: -1,
            ..HookSpec::new("first")
        },
        Arc::new(First),
    )
    .unwrap();
    let mut registry = ExtensionRegistry::new();
    registry.install(e).unwrap();
    let result = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(Arc::new(registry.finish()))
        .build()
        .unwrap()
        .run(ModelRequest::new(
            "m",
            vec![Message::text(MessageRole::User, "user")],
        ))
        .await
        .unwrap();
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert!(
        !serde_json::to_string(&result.messages)
            .unwrap()
            .contains("REMOVED_PROPOSAL")
    );
}
struct Always;
#[async_trait]
impl BoundaryHook for Always {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        Ok(
            (input.kind == BoundaryKind::TurnEnd).then(|| BoundaryAction {
                continue_run: Some(true),
                ..Default::default()
            }),
        )
    }
}
#[tokio::test]
async fn lifecycle_continuations_cannot_escape_round_or_token_limits() {
    let root = tempfile::tempdir().unwrap();
    let provider = model();
    let outcome = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(set(Arc::new(Always)))
        .config(TurnExecutionConfig {
            max_rounds: 2,
            ..Default::default()
        })
        .build()
        .unwrap()
        .run_outcome(
            ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome,RunOutcome::Interrupted(ref i)if matches!(i.stop,RunStop::Budget(ref b)if b.resource==BudgetResource::Rounds))
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    let provider = model();
    let outcome = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(set(Arc::new(Always)))
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_tokens: Some(1),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap()
        .run_outcome(
            ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    assert!(provider.requests.lock().unwrap().is_empty());
}
struct Unsupported;
#[async_trait]
impl RunJournal for Unsupported {
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
struct FailCommit;
#[async_trait]
impl RunJournal for FailCommit {
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
    async fn record_lifecycle(&self, _: &LifecycleRecord) -> Result<()> {
        Ok(())
    }
    async fn commit_boundary(&self, _: &BoundaryCommit) -> Result<()> {
        anyhow::bail!("private backend failure")
    }
}
#[tokio::test]
async fn journal_support_is_checked_before_provider_io_and_failed_commits_publish_no_context() {
    for fail_commit in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let provider = model();
        let executor = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(Arc::new(Once {
                kind: BoundaryKind::TurnEnd,
                again: true,
                calls: Arc::new(Mutex::new(vec![])),
            })))
            .build()
            .unwrap();
        let journal: &dyn RunJournal = if fail_commit {
            &FailCommit
        } else {
            &Unsupported
        };
        let outcome = executor
            .run_with_journal(
                ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
                Arc::new(()),
                TurnExecutionContext::new(),
                journal,
            )
            .await;
        assert!(
            matches!(outcome,RunOutcome::Interrupted(ref i)if matches!(&i.stop,RunStop::Failed(f)if f.stage==RunStage::Persistence))
        );
        assert_eq!(
            provider.requests.lock().unwrap().len(),
            usize::from(fail_commit)
        );
        assert!(
            !serde_json::to_string(&outcome.progress().messages)
                .unwrap()
                .contains("EXPLICIT_CONTEXT_FACT")
        );
    }
}
struct Large;
#[async_trait]
impl BoundaryHook for Large {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        Ok(
            (input.kind == BoundaryKind::TurnEnd).then(|| BoundaryAction {
                append: vec![BoundaryEntry::Context {
                    kind: "large".into(),
                    text: "x".repeat(4096),
                }],
                continue_run: Some(true),
                ..Default::default()
            }),
        )
    }
}
#[tokio::test]
async fn complete_boundary_output_is_admitted_before_commit_and_never_partially_published() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let provider = model();
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(Arc::new(Large)))
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_output_bytes: Some(256),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "user",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    let saved = repo.open(&session.metadata().id).unwrap();
    assert!(!saved.entries().iter().any(|e|matches!(&e.data,SessionEntryData::Extension{namespace,data}if namespace==LIFECYCLE_NAMESPACE&&data["type"]=="applied")));
    assert_eq!(saved.messages().len(), 2);
}
struct Bad(&'static str);
#[async_trait]
impl BoundaryHook for Bad {
    async fn boundary(
        &self,
        _: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        if self.0 == "error" {
            anyhow::bail!("PRIVATE_CALLBACK_ERROR");
        }
        if self.0 == "panic" {
            panic!("fixture panic");
        }
        Ok(Some(BoundaryAction {
            append: if self.0 == "count" {
                (0..33)
                    .map(|_| BoundaryEntry::Data {
                        key: "k".into(),
                        value: json!(1),
                    })
                    .collect()
            } else {
                vec![BoundaryEntry::Context {
                    kind: "k".into(),
                    text: "x".repeat(32769),
                }]
            },
            ..Default::default()
        }))
    }
}
#[tokio::test]
async fn invalid_or_failed_callbacks_cannot_append_late_actions_or_repeat_the_completed_model() {
    for mode in ["error", "panic", "count", "bytes"] {
        let root = tempfile::tempdir().unwrap();
        let provider = model();
        let outcome = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(Arc::new(Bad(mode))))
            .build()
            .unwrap()
            .run_outcome(
                ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
        assert_eq!(outcome.progress().usage.total_tokens, Some(7));
        assert_eq!(outcome.progress().messages.len(), 2);
        assert!(
            !outcome
                .stop()
                .unwrap()
                .to_string()
                .contains("PRIVATE_CALLBACK_ERROR")
        );
    }
}
struct Slow {
    started: Arc<tokio::sync::Notify>,
    ended: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl BoundaryHook for Slow {
    async fn boundary(
        &self,
        _: BoundaryInput<'_>,
        c: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        let end = self.ended.clone();
        tokio::spawn(async move {
            c.cancelled().await;
            end.notify_one();
        });
        self.started.notify_one();
        std::future::pending().await
    }
}
#[tokio::test]
async fn cancellation_or_timeout_drops_callback_children_and_keeps_paid_model_usage() {
    for cancel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let provider = model();
        let started = Arc::new(tokio::sync::Notify::new());
        let ended = Arc::new(tokio::sync::Notify::new());
        let mut e = Extension::new("slow", "1").unwrap();
        e.boundary_hook(
            HookSpec {
                timeout: Duration::from_millis(20),
                ..HookSpec::new("wait")
            },
            Arc::new(Slow {
                started: started.clone(),
                ended: ended.clone(),
            }),
        )
        .unwrap();
        let mut r = ExtensionRegistry::new();
        r.install(e).unwrap();
        let executor = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap();
        let context = TurnExecutionContext::new();
        let worker = context.clone();
        let run = tokio::spawn(async move {
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
            context.cancel("cancel boundary");
        }
        let outcome = run.await.unwrap();
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(outcome.progress().usage.total_tokens, Some(7));
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
        tokio::time::timeout(Duration::from_secs(1), ended.notified())
            .await
            .unwrap();
    }
}
struct Reject(Arc<AtomicUsize>);
#[async_trait]
impl CompletionVerifier for Reject {
    async fn verify(&self, _: CompletionContext<'_>) -> Result<CompletionVerdict> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(CompletionVerdict::Rejected {
            feedback: "not complete".into(),
        })
    }
}
#[tokio::test]
async fn settle_hooks_cannot_override_completion_verifier_rejection() {
    let root = tempfile::tempdir().unwrap();
    let provider = model();
    let calls = Arc::new(Mutex::new(vec![]));
    let checked = Arc::new(AtomicUsize::new(0));
    let outcome = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(set(Arc::new(Once {
            kind: BoundaryKind::BeforeSettle,
            again: false,
            calls: calls.clone(),
        })))
        .completion_verifier(Reject(checked.clone()))
        .config(TurnExecutionConfig {
            max_rounds: 2,
            ..Default::default()
        })
        .build()
        .unwrap()
        .run_outcome(
            ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    assert_eq!(checked.load(Ordering::SeqCst), 2);
    assert!(!calls.lock().unwrap().contains(&BoundaryKind::BeforeSettle));
}
struct Echo(Arc<AtomicUsize>);
#[async_trait]
impl ExecutableTool for Echo {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess::new(json!("tool result")))
    }
}
struct AfterTools;
#[async_trait]
impl BoundaryHook for AfterTools {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        if input.kind == BoundaryKind::TurnEnd && input.round == 1 {
            assert_eq!(input.statistics.tool_calls, 1);
            assert!(input.history.last().unwrap().tool_result().is_some());
            Ok(Some(BoundaryAction {
                append: vec![BoundaryEntry::Context {
                    kind: "after-tools".into(),
                    text: "post tool context".into(),
                }],
                ..Default::default()
            }))
        } else {
            Ok(None)
        }
    }
}
#[tokio::test]
async fn tool_round_boundary_runs_after_paired_results_and_before_the_next_model() {
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(Provider {
        tools: true,
        ..Default::default()
    });
    let executed = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry.register(Echo(executed.clone())).unwrap();
    let result = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .registry(registry)
        .extensions(set(Arc::new(AfterTools)))
        .build()
        .unwrap()
        .run(ModelRequest::new(
            "m",
            vec![Message::text(MessageRole::User, "user")],
        ))
        .await
        .unwrap();
    assert_eq!(executed.load(Ordering::SeqCst), 1);
    assert_eq!(result.rounds, 2);
    let requests = provider.requests.lock().unwrap();
    assert!(
        requests[1]
            .messages
            .last()
            .unwrap()
            .text_content()
            .contains("post tool context")
    );
}
#[tokio::test]
async fn explicit_context_is_summary_data_but_inert_boundary_entries_are_never_promoted() {
    let provider = model();
    let commit = BoundaryCommit {
        startup: None,
        kind: BoundaryKind::TurnEnd,
        round: 1,
        base_index: 0,
        continue_run: false,
        changes: vec![],
        entries: vec![
            BoundaryProposal {
                extension: "source".into(),
                hook: "message".into(),
                entry: BoundaryEntry::Context {
                    kind: "fact".into(),
                    text: "EXPLICIT_CONTEXT_FACT".into(),
                },
            },
            BoundaryProposal {
                extension: "source".into(),
                hook: "data".into(),
                entry: BoundaryEntry::Data {
                    key: "k".into(),
                    value: json!("INERT_DATA_NOT_PROMPTED"),
                },
            },
        ],
    };
    let mut history = vec![Message::text(MessageRole::System, "host")];
    history.extend(commit.messages());
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
    let compactor = SemanticContextCompactor::new(provider.clone(), "m", config).unwrap();
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
    let encoded = serde_json::to_string(&*provider.requests.lock().unwrap()).unwrap();
    assert!(encoded.contains("EXPLICIT_CONTEXT_FACT"));
    assert!(!encoded.contains("INERT_DATA_NOT_PROMPTED"));
}
#[tokio::test]
async fn restored_entries_do_not_restore_callbacks_or_automatic_continuations() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let provider = model();
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(Arc::new(Once {
                kind: BoundaryKind::TurnEnd,
                again: true,
                calls: Arc::new(Mutex::new(vec![])),
            })))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "first",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    let before=repo.open(&session.metadata().id).unwrap().entries().iter().filter(|e|matches!(&e.data,SessionEntryData::Extension{namespace,..}if namespace==LIFECYCLE_NAMESPACE)).count();
    let fresh_provider = model();
    let fresh = Agent::new(
        TurnExecutor::builder_from_shared(fresh_provider.clone())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    fresh
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "second",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(fresh_provider.requests.lock().unwrap().len(), 1);
    let after=repo.open(&session.metadata().id).unwrap().entries().iter().filter(|e|matches!(&e.data,SessionEntryData::Extension{namespace,..}if namespace==LIFECYCLE_NAMESPACE)).count();
    assert_eq!(before, after);
}

#[test]
fn native_context_is_explicit_extension_wire_and_plain_system_or_generic_metadata_cannot_impersonate_it()
 {
    let source = NativeContextSource {
        extension: "example".into(),
        hook: "message".into(),
        kind: "note".into(),
    };
    let native = SessionEntryData::native_context(source.clone(), "native note").unwrap();
    let encoded = serde_json::to_value(&native).unwrap();
    assert_eq!(encoded["kind"], "extension");
    assert_eq!(encoded["namespace"], NATIVE_CONTEXT_NAMESPACE);
    let restored: SessionEntryData = serde_json::from_value(encoded.clone()).unwrap();
    assert!(matches!(restored, SessionEntryData::NativeContext { .. }));
    assert_eq!(restored.as_message(), native.as_message());
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    assert!(
        repo.append(
            &mut session,
            Message::text(MessageRole::System, "ordinary system")
        )
        .is_err()
    );
    let generic = SessionEntryData::Extension {
        namespace: NATIVE_CONTEXT_NAMESPACE.into(),
        data: encoded["data"].clone(),
    };
    assert!(serde_json::to_value(&generic).is_err());
    assert!(repo.append_entries(&mut session, vec![generic]).is_err());
    let mut invalid = encoded;
    invalid["data"]["message"]["role"] = json!("user");
    assert!(serde_json::from_value::<SessionEntryData>(invalid).is_err());
}

struct WaitSettle {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    ended: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl BoundaryHook for WaitSettle {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        c: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        if input.kind != BoundaryKind::BeforeSettle || input.round != 1 {
            return Ok(None);
        }
        let end = self.ended.clone();
        tokio::spawn(async move {
            c.cancelled().await;
            end.notify_one();
        });
        self.started.notify_one();
        self.release.notified().await;
        Ok(Some(BoundaryAction {
            append: vec![BoundaryEntry::Context {
                kind: "waiting".into(),
                text: "STAGED_CONTEXT".into(),
            }],
            ..Default::default()
        }))
    }
}
#[tokio::test]
async fn queued_followup_or_steering_during_settlement_never_gets_lost_or_commits_late_proposals() {
    for steer in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let provider = model();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let ended = Arc::new(tokio::sync::Notify::new());
        let executor = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(Arc::new(WaitSettle {
                started: started.clone(),
                release: release.clone(),
                ended: ended.clone(),
            })))
            .build()
            .unwrap();
        let (handle, control) = RunHandle::pair();
        let run = tokio::spawn(async move {
            executor
                .run_controlled(
                    ModelRequest::new("m", vec![Message::text(MessageRole::User, "user")]),
                    Arc::new(()),
                    control,
                )
                .await
        });
        started.notified().await;
        if steer {
            handle
                .steer(Message::text(MessageRole::User, "new input"))
                .unwrap();
        } else {
            handle
                .follow_up(Message::text(MessageRole::User, "new input"))
                .unwrap();
            release.notify_one();
        }
        let outcome = tokio::time::timeout(Duration::from_secs(1), run)
            .await
            .unwrap()
            .unwrap()
            .into_result()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), ended.notified())
            .await
            .unwrap();
        assert_eq!(provider.requests.lock().unwrap().len(), 2);
        assert_eq!(
            provider.requests.lock().unwrap()[1]
                .messages
                .last()
                .unwrap()
                .text_content(),
            "new input"
        );
        if steer {
            assert!(
                !serde_json::to_string(&outcome.messages)
                    .unwrap()
                    .contains("STAGED_CONTEXT")
            );
        }
    }
}

struct AuxProvider;
#[async_trait]
impl aicoder_core::auxiliary::ClassifierProvider for AuxProvider {
    async fn classify(
        &self,
        _: &aicoder_core::auxiliary::ClassifierModel,
        _: &aicoder_core::auxiliary::ClassifierContext,
        usage: aicoder_core::auxiliary::AuxiliaryUsageReporter,
        _: TurnExecutionContext,
    ) -> Result<aicoder_core::auxiliary::ClassifierAnswers, ProviderError> {
        usage.record(ModelUsage {
            input_tokens: Some(11),
            output_tokens: Some(0),
            total_tokens: Some(11),
            cached_input_tokens: Some(0),
            cache_write_tokens: Some(0),
        });
        Ok(serde_json::from_value(json!({"ok":{"type":"bool","probability":0.9}})).unwrap())
    }
}
struct AuxTool;
struct AuxPlan {
    model: aicoder_core::auxiliary::ClassifierModel,
    context: aicoder_core::auxiliary::ClassifierContext,
}
#[async_trait]
impl ExecutableTool for AuxTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Command
    }
    async fn prepare(
        &self,
        _: &ToolContext,
        _: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        Ok(Some(Box::new(AuxPlan{model:serde_json::from_value(json!({"provider":"aux","id":"native"})).unwrap(),context:serde_json::from_value(json!({"state":{"text":"sample"},"questions":{"ok":{"type":"bool","instructions":"valid?","criteria":{"true":"yes","false":"no"}}}})).unwrap()})))
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        panic!("prepared metered operation required")
    }
}
#[async_trait]
impl PreparedTool for AuxPlan {
    fn preview(&self) -> ToolPreview {
        ToolPreview {
            summary: "native".into(),
            body: "bounded".into(),
            format: "text".into(),
            truncated: false,
        }
    }
    fn auxiliary_operation(&self) -> Option<&dyn aicoder_core::auxiliary::AuxiliaryOperation> {
        Some(self)
    }
    async fn execute(self: Box<Self>, _: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        panic!("core metered dispatch required")
    }
}
#[async_trait]
impl aicoder_core::auxiliary::AuxiliaryOperation for AuxPlan {
    fn model(&self) -> &aicoder_core::auxiliary::ClassifierModel {
        &self.model
    }
    fn context(&self) -> &aicoder_core::auxiliary::ClassifierContext {
        &self.context
    }
    fn provider(&self) -> &dyn aicoder_core::auxiliary::ClassifierProvider {
        &AuxProvider
    }
    async fn deliver(
        &self,
        answers: aicoder_core::auxiliary::ClassifierAnswers,
        _: Option<ModelUsage>,
        _: &ToolContext,
    ) -> Result<ToolSuccess, ToolFailure> {
        Ok(ToolSuccess::new(json!(answers)))
    }
}
struct UsageAudit;
#[async_trait]
impl BoundaryHook for UsageAudit {
    async fn boundary(
        &self,
        input: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        assert_eq!(input.statistics.auxiliary.len(), 1);
        assert_eq!(
            input
                .statistics
                .primary_usage
                .as_ref()
                .unwrap()
                .total_tokens,
            Some(input.round as u64 * 7)
        );
        assert_eq!(
            input.statistics.usage.total_tokens,
            Some(input.round as u64 * 7 + 11)
        );
        Ok(None)
    }
}
#[tokio::test]
async fn boundary_statistics_include_native_and_primary_usage_without_mutating_or_double_charging_progress()
 {
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(Provider {
        tools: true,
        ..Default::default()
    });
    let mut registry = ToolRegistry::default();
    registry.register(AuxTool).unwrap();
    let result = TurnExecutor::builder_from_shared(provider)
        .workspace(root.path())
        .registry(registry)
        .approval(AllowAllApproval)
        .extensions(set(Arc::new(UsageAudit)))
        .build()
        .unwrap()
        .run(ModelRequest::new(
            "m",
            vec![Message::text(MessageRole::User, "user")],
        ))
        .await
        .unwrap();
    assert_eq!(result.usage.total_tokens, Some(25));
    assert_eq!(
        result.primary_usage.as_ref().unwrap().total_tokens,
        Some(14)
    );
}
struct SlowCommit;
#[async_trait]
impl RunJournal for SlowCommit {
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn record_lifecycle(&self, _: &LifecycleRecord) -> Result<()> {
        Ok(())
    }
    async fn commit_boundary(&self, _: &BoundaryCommit) -> Result<()> {
        std::thread::sleep(Duration::from_millis(150));
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn acknowledged_boundary_facts_survive_a_deadline_before_any_requested_continuation() {
    let root = tempfile::tempdir().unwrap();
    let provider = model();
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(set(Arc::new(Once {
            kind: BoundaryKind::TurnEnd,
            again: true,
            calls: Arc::new(Mutex::new(vec![])),
        })))
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_duration: Some(Duration::from_millis(75)),
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
            &SlowCommit,
        )
        .await;
    assert!(matches!(outcome,RunOutcome::Interrupted(ref i)if matches!(i.stop,RunStop::Budget(_))));
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert!(
        outcome
            .progress()
            .messages
            .iter()
            .any(|m| m.text_content().contains("EXPLICIT_CONTEXT_FACT"))
    );
}
