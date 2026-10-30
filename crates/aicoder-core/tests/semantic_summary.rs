use aicoder_core::{context::*, model::*, session::*, summary::*, *};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy)]
enum Reply {
    Valid,
    Invalid,
    Fail,
    Cancel,
    Unknown,
    Wait,
}
#[derive(Default)]
struct Script {
    requests: Mutex<Vec<ModelRequest>>,
    replies: Mutex<VecDeque<Reply>>,
}
impl Script {
    fn new(replies: &[Reply]) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.iter().copied().collect()),
            ..Self::default()
        })
    }
    fn summaries(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| is_summary(r))
            .count()
    }
}
fn is_summary(request: &ModelRequest) -> bool {
    request
        .messages
        .first()
        .is_some_and(|m| m.text_content().starts_with("Summarize historical"))
}
fn usage() -> ModelUsage {
    ModelUsage {
        input_tokens: Some(8),
        output_tokens: Some(2),
        total_tokens: Some(10),
        ..Default::default()
    }
}
fn summary() -> ConversationSummary {
    ConversationSummary {
        goals: vec!["implement feature".into()],
        constraints: vec!["keep dependencies small".into()],
        changes: vec!["src/lib.rs updated".into()],
        todos: vec!["run tests".into()],
        notes: vec![],
    }
}
#[async_trait]
impl ModelProvider for Script {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("scripted", model);
        spec.context_tokens = Some(16384);
        spec.max_output_tokens = Some(1024);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        context: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let summarizing = is_summary(&request);
        self.requests.lock().unwrap().push(request.clone());
        let reply = if summarizing {
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Reply::Valid)
        } else {
            Reply::Valid
        };
        if matches!(reply, Reply::Fail | Reply::Cancel) {
            sink.emit(ModelEvent::Usage { usage: usage() });
            if matches!(reply, Reply::Cancel) {
                context.cancel("script cancellation");
            }
            return Err(ProviderError::new(
                "scripted",
                if matches!(reply, Reply::Cancel) {
                    ProviderErrorKind::Cancelled
                } else {
                    ProviderErrorKind::Other
                },
                "scripted failure",
            ));
        }
        if matches!(reply, Reply::Wait) {
            sink.emit(ModelEvent::Usage { usage: usage() });
            return std::future::pending().await;
        }
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(
                MessageRole::Assistant,
                if !summarizing {
                    "done".into()
                } else if matches!(reply, Reply::Invalid) {
                    "not JSON".into()
                } else {
                    serde_json::to_string(&summary()).unwrap()
                },
            ),
            stop_reason: StopReason::Complete,
            usage: if matches!(reply, Reply::Unknown) {
                ModelUsage::default()
            } else {
                usage()
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
fn config() -> SummaryConfig {
    let mut c = SummaryConfig::new(ContextWindowConfig {
        max_context_tokens: 1200,
        reserved_output_tokens: 200,
        preserve_recent_tokens: 80,
    });
    c.output_tokens = 128;
    c.max_input_tokens = 1400;
    c
}
fn history() -> Vec<Message> {
    let mut result = vec![Message::text(MessageRole::System, "trusted prefix")];
    result.extend((0..8).map(|i| {
        Message::text(
            if i % 2 == 0 {
                MessageRole::User
            } else {
                MessageRole::Assistant
            },
            format!("old-{i} {}", "x".repeat(1000)),
        )
    }));
    result.push(Message::text(MessageRole::User, "newest request"));
    result
}
async fn compact(
    compactor: &SemanticContextCompactor,
    messages: &[Message],
    force: bool,
    budget: Option<u64>,
) -> anyhow::Result<RecordedCompaction> {
    compactor
        .compact_recorded(
            CompactionRequest {
                input: ContextCompactionInput {
                    model: "test",
                    messages,
                    tools: None,
                    max_output_tokens: None,
                },
                history: messages,
                system_prefix_len: 1,
                force,
                token_budget: budget,
                archived_messages: 0,
            },
            TurnExecutionContext::new(),
        )
        .await
}
fn agent(provider: Arc<Script>, root: &std::path::Path, c: SummaryConfig) -> Agent {
    Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root)
            .context_compactor(SemanticContextCompactor::new(provider, "test", c).unwrap())
            .build()
            .unwrap(),
        AgentConfig::new("test").system_prompt("trusted prefix"),
    )
}
fn seed(repo: &dyn SessionRepository, root: &std::path::Path) -> Session {
    let mut session = repo.create(root).unwrap();
    for message in history().into_iter().skip(1) {
        repo.append(&mut session, message).unwrap();
    }
    session
}
#[tokio::test]
async fn chunks_merge_structured_summary_and_count_every_attempt() {
    let provider = Script::new(&[Reply::Invalid, Reply::Valid]);
    let compactor = SemanticContextCompactor::new(provider.clone(), "test", config()).unwrap();
    let messages = history();
    let original = messages.clone();
    let result = compact(&compactor, &messages, false, None).await.unwrap();
    let draft = result.checkpoint.unwrap();
    assert_eq!(messages, original);
    assert!(draft.attempts >= 3, "retry plus multiple chunks");
    assert_eq!(
        result.compaction.usage.total_tokens,
        Some(draft.attempts as u64 * 10)
    );
    assert_eq!(result.accounting.unwrap().used, draft.attempts as u64 * 10);
    assert_eq!(result.compaction.messages[0], messages[0]);
    assert_eq!(result.compaction.messages.last(), messages.last());
    assert_eq!(draft.summary.constraints, ["keep dependencies small"]);
    assert!(result.compaction.estimated_tokens_after <= 1000);
    let requests = provider.requests.lock().unwrap();
    for request in requests.iter() {
        assert!(request.tools.is_empty());
        assert_eq!(request.tool_choice, ModelToolChoice::None);
        assert!(estimate_context_tokens(&request.messages, None) <= 1400);
    }
    assert!(requests.iter().skip(2).any(|r| {
        r.messages[1]
            .text_content()
            .contains("keep dependencies small")
    }));
}
#[tokio::test]
async fn fallback_is_transient_cached_and_manual_retry_bypasses_cache() {
    let provider = Script::new(&[Reply::Fail, Reply::Invalid]);
    let compactor = SemanticContextCompactor::new(provider.clone(), "test", config()).unwrap();
    let messages = history();
    let fallback = compact(&compactor, &messages, false, None).await.unwrap();
    assert!(fallback.checkpoint.is_none());
    assert_eq!(fallback.attempts, 2);
    assert_eq!(fallback.compaction.usage.total_tokens, Some(20));
    assert_eq!(fallback.diagnostics.len(), 1);
    let cached = compact(&compactor, &messages, false, None).await.unwrap();
    assert_eq!(cached.attempts, 0);
    assert_eq!(provider.summaries(), 2);
    let manual = compact(&compactor, &messages, true, None).await.unwrap();
    assert!(manual.checkpoint.is_some());
    assert!(provider.summaries() > 2);
}
#[tokio::test]
async fn budget_preflight_calls_nothing_and_partial_budget_preserves_consumed_usage() {
    let provider = Script::new(&[]);
    let compactor = SemanticContextCompactor::new(provider.clone(), "test", config()).unwrap();
    let messages = history();
    let error = compact(&compactor, &messages, false, Some(1))
        .await
        .err()
        .unwrap();
    let failure = error.downcast_ref::<SummaryFailure>().unwrap();
    assert_eq!(failure.kind, SummaryFailureKind::Budget);
    assert_eq!(failure.attempts, 0);
    assert_eq!(provider.summaries(), 0);
    let provider = Script::new(&[Reply::Unknown]);
    let compactor = SemanticContextCompactor::new(provider, "test", config()).unwrap();
    let error = compact(&compactor, &messages, false, Some(1500))
        .await
        .err()
        .unwrap();
    let failure = error.downcast_ref::<SummaryFailure>().unwrap();
    assert_eq!(failure.kind, SummaryFailureKind::Budget);
    assert!(failure.tokens.used > 0);
    assert!(failure.tokens.estimated);
}
#[tokio::test]
async fn cancellation_is_terminal_and_keeps_partial_usage() {
    let provider = Script::new(&[Reply::Cancel]);
    let compactor = SemanticContextCompactor::new(provider.clone(), "test", config()).unwrap();
    let error = compact(&compactor, &history(), false, None)
        .await
        .err()
        .unwrap();
    let failure = error.downcast_ref::<SummaryFailure>().unwrap();
    assert_eq!(failure.kind, SummaryFailureKind::Cancelled);
    assert_eq!(failure.usage.total_tokens, Some(10));
    assert_eq!(provider.summaries(), 1);
}
#[tokio::test]
async fn stop_policy_and_call_limit_do_not_publish_partial_summary() {
    let provider = Script::new(&[]);
    let mut c = config();
    c.failure_policy = SummaryFailurePolicy::Stop;
    c.max_calls = 1;
    let compactor = SemanticContextCompactor::new(provider, "test", c).unwrap();
    let error = compact(&compactor, &history(), false, None)
        .await
        .err()
        .unwrap();
    let failure = error.downcast_ref::<SummaryFailure>().unwrap();
    assert_eq!(failure.kind, SummaryFailureKind::SourceLimit);
    assert_eq!(failure.attempts, 1);
    assert_eq!(failure.usage.total_tokens, Some(10));
}
#[tokio::test]
async fn opaque_provider_state_is_omitted_from_summary_without_rewriting_history() {
    let provider = Script::new(&[]);
    let compactor = SemanticContextCompactor::new(provider.clone(), "test", config()).unwrap();
    let mut messages = history();
    messages[2].content.push(ContentBlock::ProviderData {
        provider: "scripted".into(),
        data: json!({"secret":"OPAQUE_SENTINEL"}),
    });
    messages[2].content.push(ContentBlock::Reasoning {
        text: "REASONING_SENTINEL".into(),
    });
    compact(&compactor, &messages, false, None).await.unwrap();
    assert!(
        serde_json::to_string(&messages)
            .unwrap()
            .contains("OPAQUE_SENTINEL")
    );
    assert!(
        !serde_json::to_string(&*provider.requests.lock().unwrap())
            .unwrap()
            .contains("SENTINEL")
    );
}
#[tokio::test]
async fn manual_summary_is_branch_local_durable_and_preserves_raw_history() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = seed(&repo, root.path());
    let before = session.selected_branch().chat_messages();
    let old_leaf = session.metadata().leaf_id.clone();
    let provider = Script::new(&[]);
    let agent = agent(provider.clone(), root.path(), config());
    let report = agent
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(report.succeeded(), "{report:?}");
    assert!(report.checkpoint_entry_id.is_some());
    let mut restored = repo.open(&session.metadata().id).unwrap();
    assert_eq!(restored.selected_branch().chat_messages(), before);
    assert!(!restored.entries().iter().any(|entry| matches!(
        entry.data,
        SessionEntryData::TurnStart { .. } | SessionEntryData::TurnEnd { .. }
    )));
    let projected = restored
        .selected_branch()
        .model_context()
        .unwrap()
        .into_messages();
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0].role, MessageRole::User);
    assert!(
        projected[0]
            .text_content()
            .contains("keep dependencies small")
    );
    assert_eq!(
        restored.selected_branch().accounting().unwrap().usage,
        report.usage
    );
    repo.checkout(&mut restored, old_leaf.as_deref()).unwrap();
    assert_eq!(
        restored.selected_branch().model_context().unwrap().messages,
        before
    );
    assert_eq!(
        restored.selected_branch().accounting().unwrap().usage,
        ModelUsage::zero()
    );
    repo.append(&mut restored, Message::text(MessageRole::User, "sibling"))
        .unwrap();
    assert!(
        restored
            .selected_branch()
            .model_context()
            .unwrap()
            .source
            .summary_checkpoint
            .is_none()
    );
}
#[tokio::test]
async fn automatic_checkpoint_is_replayed_without_double_charging() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = seed(&repo, root.path());
    let provider = Script::new(&[]);
    let agent = agent(provider.clone(), root.path(), config());
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "continue",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "{outcome:?}"
    );
    let attempts = provider.summaries();
    assert!(attempts > 0);
    let saved = repo.open(&session.metadata().id).unwrap();
    assert_eq!(
        saved
            .selected_branch()
            .accounting()
            .unwrap()
            .usage
            .total_tokens,
        Some((attempts as u64 + 1) * 10)
    );
    assert!(
        saved
            .selected_branch()
            .model_context()
            .unwrap()
            .source
            .summary_checkpoint
            .is_some()
    );
    assert_eq!(saved.messages().len(), session.messages().len() + 2);
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "next",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "{outcome:?}"
    );
    assert_eq!(provider.summaries(), attempts);
    let saved = repo.open(&session.metadata().id).unwrap();
    assert_eq!(
        saved
            .selected_branch()
            .accounting()
            .unwrap()
            .usage
            .total_tokens,
        Some((attempts as u64 + 2) * 10)
    );
}
#[tokio::test]
async fn failed_manual_summary_records_usage_but_keeps_projection() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = seed(&repo, root.path());
    let provider = Script::new(&[Reply::Invalid, Reply::Fail]);
    let agent = agent(provider, root.path(), config());
    let report = agent
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(!report.succeeded());
    assert!(report.checkpoint_entry_id.is_none());
    assert_eq!(report.usage.total_tokens, Some(20));
    let saved = repo.open(&session.metadata().id).unwrap();
    assert_eq!(saved.messages().len(), session.messages().len());
    assert!(
        saved
            .selected_branch()
            .model_context()
            .unwrap()
            .source
            .summary_checkpoint
            .is_none()
    );
    assert_eq!(
        saved
            .selected_branch()
            .accounting()
            .unwrap()
            .usage
            .total_tokens,
        Some(20)
    );
}
#[tokio::test]
async fn checkpoint_rejects_foreign_anchor_or_incorrect_tail_transactionally() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = seed(&repo, root.path());
    let agent = agent(Script::new(&[]), root.path(), config());
    let report = agent
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(report.succeeded(), "{report:?}");
    let mut saved = repo.open(&session.metadata().id).unwrap();
    let SessionEntryData::Extension { data, .. } = &saved.entries().last().unwrap().data else {
        panic!()
    };
    let original: SummaryCheckpoint = serde_json::from_value(data.clone()).unwrap();
    for mutate in 0..3 {
        let mut invalid = original.clone();
        invalid.source_leaf = saved.metadata().leaf_id.clone().unwrap();
        match mutate {
            0 => invalid.first_kept_entry_id = "foreign".into(),
            1 => invalid.draft.keep_messages += 1,
            _ => invalid.accounted_by_turn = Some("foreign".into()),
        }
        let before = saved.metadata().clone();
        assert!(
            repo.append_entries(&mut saved, vec![invalid.data()])
                .is_err()
        );
        assert_eq!(saved.metadata(), &before);
    }
}

#[tokio::test]
async fn newest_tool_unit_is_preserved_whole() {
    let provider = Script::new(&[]);
    let compactor = SemanticContextCompactor::new(provider, "test", config()).unwrap();
    let mut messages = history();
    messages.pop();
    messages.push(Message {
        role: MessageRole::Assistant,
        content: vec![ContentBlock::ToolCall {
            call: ModelToolCall {
                id: "latest".into(),
                name: "read".into(),
                arguments: json!({}),
            },
        }],
    });
    messages.push(Message {
        role: MessageRole::Tool,
        content: vec![ContentBlock::ToolResult {
            call_id: "latest".into(),
            content: json!({"ok":true}),
            is_error: false,
        }],
    });
    let result = compact(&compactor, &messages, false, None).await.unwrap();
    assert_eq!(result.checkpoint.unwrap().keep_messages, 2);
    assert!(
        result
            .compaction
            .messages
            .ends_with(&messages[messages.len() - 2..])
    );
}
#[tokio::test]
async fn summarized_interruption_stays_visible_after_restore() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::TurnStart {
                turn_id: "crashed".into(),
            },
            SessionEntryData::Message {
                message: Message::text(MessageRole::User, "old operation"),
            },
            SessionEntryData::TurnEnd {
                turn_id: "crashed".into(),
                record: Box::new(TurnRecord::recovered()),
            },
        ],
    )
    .unwrap();
    for message in history().into_iter().skip(1) {
        repo.append(&mut session, message).unwrap();
    }
    let agent = agent(Script::new(&[]), root.path(), config());
    let report = agent
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(report.succeeded(), "{report:?}");
    let saved = repo.open(&session.metadata().id).unwrap();
    let projected = saved
        .selected_branch()
        .model_context()
        .unwrap()
        .into_messages();
    assert_eq!(projected[0], recovery_notice());
    assert!(
        projected[1]
            .text_content()
            .contains("keep dependencies small")
    );
    assert!(
        !serde_json::to_string(saved.entries())
            .unwrap()
            .contains("Earlier summarized executions")
    );
}
struct RejectSummary;
#[async_trait]
impl RunJournal for RejectSummary {
    async fn checkpoint(&self, _: usize, _: &Message) -> anyhow::Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> anyhow::Result<()> {
        Ok(())
    }
    async fn record_summary(
        &self,
        _: &SummaryDraft,
        _: usize,
        _: usize,
    ) -> anyhow::Result<Option<String>> {
        anyhow::bail!("checkpoint storage unavailable")
    }
}
#[tokio::test]
async fn failed_checkpoint_stops_before_main_model_and_retains_summary_cost() {
    let root = tempfile::tempdir().unwrap();
    let provider = Script::new(&[]);
    let exec = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .context_compactor(
            SemanticContextCompactor::new(provider.clone(), "test", config()).unwrap(),
        )
        .build()
        .unwrap();
    let outcome = exec
        .run_with_journal(
            ModelRequest::new("test", history()),
            Arc::new(()),
            TurnExecutionContext::new(),
            &RejectSummary,
        )
        .await;
    assert!(matches!(outcome.stop(), Some(RunStop::Failed(_))));
    assert_eq!(
        outcome.progress().usage.total_tokens,
        Some(provider.summaries() as u64 * 10)
    );
    assert!(provider.requests.lock().unwrap().iter().all(is_summary));
    assert_eq!(outcome.progress().messages, history());
}
#[tokio::test]
async fn small_context_is_free_and_oversized_source_falls_back_without_model() {
    let provider = Script::new(&[]);
    let compactor = SemanticContextCompactor::new(provider.clone(), "test", config()).unwrap();
    let short = vec![
        Message::text(MessageRole::System, "trusted"),
        Message::text(MessageRole::User, "short"),
    ];
    let result = compact(&compactor, &short, false, None).await.unwrap();
    assert_eq!(result.compaction.messages, short);
    assert!(result.checkpoint.is_none());
    let mut large = short.clone();
    large.insert(
        1,
        Message::text(MessageRole::User, "x".repeat(2 * 1024 * 1024)),
    );
    let result = compact(&compactor, &large, false, None).await.unwrap();
    assert!(result.checkpoint.is_none());
    assert_eq!(result.attempts, 0);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(provider.summaries(), 0);
    assert_eq!(result.compaction.messages.last(), short.last());
}

#[tokio::test]
async fn manual_duration_limit_records_partial_usage_and_releases_lease() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = seed(&repo, root.path());
    let provider = Script::new(&[Reply::Wait]);
    let execution = TurnExecutionConfig {
        budget: RunBudget {
            max_duration: Some(std::time::Duration::from_millis(20)),
            ..Default::default()
        },
        ..Default::default()
    };
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .config(execution)
            .context_compactor(SemanticContextCompactor::new(provider, "test", config()).unwrap())
            .build()
            .unwrap(),
        AgentConfig::new("test"),
    );
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        agent.compact_session(&repo, &session.metadata().id, TurnExecutionContext::new()),
    )
    .await
    .unwrap();
    assert!(
        matches!(report.stop, Some(RunStop::Budget(_))),
        "{report:?}"
    );
    assert_eq!(report.usage.total_tokens, Some(10));
    let mut saved = repo.open(&session.metadata().id).unwrap();
    assert!(
        saved
            .selected_branch()
            .model_context()
            .unwrap()
            .source
            .summary_checkpoint
            .is_none()
    );
    assert_eq!(
        saved
            .selected_branch()
            .accounting()
            .unwrap()
            .usage
            .total_tokens,
        Some(10)
    );
    assert!(repo.acquire_run(&mut saved).is_ok());
}
#[tokio::test]
async fn summary_and_fallback_emit_valid_traces_with_complete_accounting() {
    for replies in [vec![], vec![Reply::Invalid, Reply::Fail]] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let session = seed(&repo, root.path());
        let provider = Script::new(&replies);
        let agent = agent(provider.clone(), root.path(), config());
        let frames = Arc::new(Mutex::new(Vec::new()));
        let output = frames.clone();
        let outcome = agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "continue",
                Arc::new(move |frame: &AgentRawEventEnvelope| {
                    output.lock().unwrap().push(frame.clone())
                }),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(
            matches!(outcome, AgentTurnOutcome::Completed(_)),
            "{outcome:?}"
        );
        let mut validator = events::EventTraceValidator::new();
        for frame in frames.lock().unwrap().iter() {
            validator.observe(frame).unwrap();
        }
        validator.finish().unwrap();
        let info = outcome.progress().model_context.as_ref().unwrap();
        assert_eq!(info.summary_attempts, provider.summaries());
        assert_eq!(info.summary_fallbacks, usize::from(!replies.is_empty()));
        let saved = repo.open(&session.metadata().id).unwrap();
        assert_eq!(
            saved.selected_branch().accounting().unwrap().usage,
            outcome.progress().usage
        );
        if let Some(id) = &info.last_summary_checkpoint {
            let accounting = saved.branch(Some(id)).unwrap().accounting().unwrap();
            assert_eq!(
                accounting.usage.total_tokens,
                Some(provider.summaries() as u64 * 10)
            );
        }
    }
}

#[tokio::test]
async fn pruning_fallback_preserves_interruption_markers() {
    let provider = Script::new(&[Reply::Invalid, Reply::Invalid]);
    let compactor = SemanticContextCompactor::new(provider, "test", config()).unwrap();
    let mut messages = history();
    messages.insert(2, recovery_notice());
    let result = compact(&compactor, &messages, false, None).await.unwrap();
    assert!(result.checkpoint.is_none());
    assert!(result.compaction.messages.contains(&recovery_notice()));
}

#[tokio::test]
async fn growing_branch_merges_previous_summary_without_resending_replaced_history() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = seed(&repo, root.path());
    let provider = Script::new(&[]);
    let agent = agent(provider.clone(), root.path(), config());
    let first = agent
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(first.succeeded(), "{first:?}");
    let prior_attempts = provider.summaries();
    let mut saved = repo.open(&session.metadata().id).unwrap();
    for i in 0..8 {
        repo.append(
            &mut saved,
            Message::text(MessageRole::User, format!("fresh-{i} {}", "y".repeat(1000))),
        )
        .unwrap();
    }
    let outcome = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "continue",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "{outcome:?}"
    );
    let restored = repo.open(&session.metadata().id).unwrap();
    assert_eq!(restored.messages().len(), session.messages().len() + 10);
    let checkpoint = restored
        .selected_branch()
        .model_context()
        .unwrap()
        .source
        .summary_checkpoint;
    assert!(checkpoint.is_some() && checkpoint != first.checkpoint_entry_id);
    let summaries = provider.summaries();
    let requests = provider.requests.lock().unwrap();
    let next = requests
        .iter()
        .filter(|r| is_summary(r))
        .skip(prior_attempts)
        .collect::<Vec<_>>();
    assert!(!next.is_empty());
    let text = serde_json::to_string(&next).unwrap();
    assert!(text.contains("keep dependencies small"));
    assert!(text.contains("Historical conversation summary"));
    assert!(!text.contains("old-0"));
    assert_eq!(
        restored
            .selected_branch()
            .accounting()
            .unwrap()
            .usage
            .total_tokens,
        Some((summaries as u64 + 1) * 10)
    );
}
