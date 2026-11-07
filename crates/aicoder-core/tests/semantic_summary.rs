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
    request.messages.first().is_some_and(|m| {
        m.text_content().starts_with("Summarize historical")
            || m.text_content().starts_with("Return ONLY a JSON object")
    })
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

fn context_edit(
    repo: &dyn SessionRepository,
    session: &Session,
    target: &str,
    replacement: ContextReplacement,
) {
    SessionNavigator::new(repo, &session.metadata().cwd)
        .unwrap()
        .execute(
            &session.metadata().id,
            ArchiveNavigation::EditContext {
                edit: ContextEditRecord {
                    version: 1,
                    extension: "summary-test".into(),
                    hook: "view".into(),
                    target_id: target.into(),
                    replacement,
                },
            },
        )
        .unwrap();
}
fn checkpoint(session: &Session) -> SummaryCheckpoint {
    session
        .entries()
        .iter()
        .rev()
        .find_map(|e| match &e.data {
            SessionEntryData::Extension { namespace, data } if namespace == SUMMARY_NAMESPACE => {
                Some(serde_json::from_value(data.clone()).unwrap())
            }
            _ => None,
        })
        .unwrap()
}
#[tokio::test]
async fn manual_summary_retains_latest_edited_id_without_a_new_canonical_suffix() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = seed(&repo, root.path());
    let target = session.metadata().leaf_id.clone().unwrap();
    let raw = session.selected_branch().chat_messages();
    context_edit(
        &repo,
        &session,
        &target,
        ContextReplacement::Text("edited latest tail".into()),
    );
    let provider = Script::new(&[]);
    let mut c = config();
    c.failure_policy = SummaryFailurePolicy::Stop;
    let a = agent(provider.clone(), root.path(), c);
    let report = a
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(
        report.succeeded() && report.checkpoint_entry_id.is_some(),
        "{report:?}"
    );
    let restored = repo.open(&session.metadata().id).unwrap();
    let cp = checkpoint(&restored);
    assert_eq!(cp.version, 2);
    assert_eq!(cp.first_kept_entry_id, target);
    assert_eq!(cp.draft.keep_messages, 1);
    assert_eq!(restored.selected_branch().chat_messages(), raw);
    assert_eq!(
        restored
            .selected_branch()
            .model_context()
            .unwrap()
            .messages
            .last()
            .unwrap()
            .text_content(),
        "edited latest tail"
    );
    assert_eq!(report.usage.total_tokens, Some(report.attempts as u64 * 10));
}
#[tokio::test]
async fn summary_v2_counts_custom_and_native_messages_and_accepts_them_as_tail_anchors() {
    for native in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let mut session = seed(&repo, root.path());
        let data = if native {
            SessionEntryData::native_context(
                NativeContextSource {
                    extension: "source".into(),
                    hook: "hook".into(),
                    kind: "context".into(),
                },
                "keep native",
            )
            .unwrap()
        } else {
            SessionEntryData::CustomMessage {
                custom: CustomMessage::text("source", "kind", "keep custom").unwrap(),
            }
        };
        repo.append_entries(&mut session, vec![data]).unwrap();
        let target = session.metadata().leaf_id.clone().unwrap();
        let raw = session.selected_branch().chat_messages();
        let mut c = config();
        c.window.preserve_recent_tokens = 1;
        let report = agent(Script::new(&[]), root.path(), c)
            .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
            .await;
        assert!(
            report.succeeded() && report.checkpoint_entry_id.is_some(),
            "{report:?}"
        );
        let restored = repo.open(&session.metadata().id).unwrap();
        let cp = checkpoint(&restored);
        assert_eq!(cp.version, 2);
        assert_eq!(cp.first_kept_entry_id, target);
        assert_eq!(cp.summarized_messages, 9);
        assert_eq!(cp.draft.keep_messages, 1);
        let view = restored.selected_branch().model_context().unwrap().messages;
        assert_eq!(view.last().unwrap(), raw.last().unwrap());
        assert_eq!(restored.selected_branch().chat_messages(), raw);
        // Native context is an explicit sourced System message, not a runtime prefix.
        assert_eq!(
            restored
                .selected_branch()
                .model_context()
                .unwrap()
                .source
                .recorded_messages,
            10
        );
    }
}
#[tokio::test]
async fn summary_maps_reordered_split_results_to_the_whole_raw_tool_unit() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = seed(&repo, root.path());
    repo.append(
        &mut session,
        Message {
            role: MessageRole::Assistant,
            content: ["a", "b"]
                .into_iter()
                .map(|id| ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: id.into(),
                        name: "read".into(),
                        arguments: json!({}),
                    },
                })
                .collect(),
        },
    )
    .unwrap();
    let call_id = session.metadata().leaf_id.clone().unwrap();
    repo.append(
        &mut session,
        Message {
            role: MessageRole::Tool,
            content: ["b", "a"]
                .into_iter()
                .map(|id| ContentBlock::ToolResult {
                    call_id: id.into(),
                    content: json!({"output":id}),
                    is_error: false,
                })
                .collect(),
        },
    )
    .unwrap();
    let result_id = session.metadata().leaf_id.clone().unwrap();
    context_edit(
        &repo,
        &session,
        &result_id,
        ContextReplacement::Text("edited tool output".into()),
    );
    let raw = repo
        .open(&session.metadata().id)
        .unwrap()
        .selected_branch()
        .chat_messages();
    let mut c = config();
    c.failure_policy = SummaryFailurePolicy::Stop;
    c.window.preserve_recent_tokens = 1;
    let report = agent(Script::new(&[]), root.path(), c)
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(
        report.succeeded() && report.checkpoint_entry_id.is_some(),
        "{report:?}"
    );
    let saved = repo.open(&session.metadata().id).unwrap();
    let cp = checkpoint(&saved);
    assert_eq!(cp.first_kept_entry_id, call_id);
    assert_eq!(cp.draft.keep_messages, 2);
    let view = saved.selected_branch().model_context().unwrap().messages;
    assert_eq!(
        view.iter().filter(|m| m.role == MessageRole::Tool).count(),
        2
    );
    for (m, id) in view
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .zip(["a", "b"])
    {
        assert!(
            matches!(&m.content[0],ContentBlock::ToolResult {call_id,content,..} if call_id==id && content["output"]=="edited tool output")
        );
    }
    assert_eq!(saved.selected_branch().chat_messages(), raw);
}
#[tokio::test]
async fn omitted_duplicate_tail_keeps_the_visible_id_and_raw_suffix_count() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = seed(&repo, root.path());
    repo.append(&mut session, Message::text(MessageRole::User, "duplicate"))
        .unwrap();
    let keep = session.metadata().leaf_id.clone().unwrap();
    repo.append(&mut session, Message::text(MessageRole::User, "duplicate"))
        .unwrap();
    let omit = session.metadata().leaf_id.clone().unwrap();
    context_edit(&repo, &session, &omit, ContextReplacement::Omit);
    let mut c = config();
    c.window.preserve_recent_tokens = 1;
    c.failure_policy = SummaryFailurePolicy::Stop;
    let report = agent(Script::new(&[]), root.path(), c)
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(
        report.succeeded() && report.checkpoint_entry_id.is_some(),
        "{report:?}"
    );
    let saved = repo.open(&session.metadata().id).unwrap();
    let cp = checkpoint(&saved);
    assert_eq!(cp.first_kept_entry_id, keep);
    assert_eq!(cp.draft.keep_messages, 2);
    assert_eq!(
        saved
            .selected_branch()
            .model_context()
            .unwrap()
            .messages
            .len(),
        2
    );
    assert_eq!(saved.messages().len(), 11);
}
#[tokio::test]
async fn automatic_summary_retains_an_edited_historical_unit_with_the_new_input() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = seed(&repo, root.path());
    let target = session.metadata().leaf_id.clone().unwrap();
    context_edit(
        &repo,
        &session,
        &target,
        ContextReplacement::Text("edited historical tail".into()),
    );
    let mut c = config();
    c.window.preserve_recent_tokens = 160;
    c.failure_policy = SummaryFailurePolicy::Stop;
    let provider = Script::new(&[]);
    let a = agent(provider.clone(), root.path(), c);
    let outcome = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "fresh input",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "{outcome:?}"
    );
    let saved = repo.open(&session.metadata().id).unwrap();
    let cp = checkpoint(&saved);
    assert_eq!(cp.first_kept_entry_id, target);
    assert_eq!(cp.draft.keep_messages, 2);
    let requests = provider.requests.lock().unwrap();
    let main = requests.iter().find(|r| !is_summary(r)).unwrap();
    assert!(
        main.messages
            .iter()
            .any(|m| m.text_content() == "edited historical tail")
    );
    assert!(
        main.messages
            .iter()
            .any(|m| m.text_content() == "fresh input")
    );
    assert_eq!(saved.messages().len(), 11);
}
#[tokio::test]
async fn cancelled_projected_summary_retains_usage_and_effective_view_without_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = seed(&repo, root.path());
    let target = session.metadata().leaf_id.clone().unwrap();
    context_edit(
        &repo,
        &session,
        &target,
        ContextReplacement::Text("edited tail".into()),
    );
    let before = repo
        .open(&session.metadata().id)
        .unwrap()
        .selected_branch()
        .model_context()
        .unwrap()
        .messages;
    let report = agent(Script::new(&[Reply::Cancel]), root.path(), config())
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(!report.succeeded());
    assert_eq!(report.usage.total_tokens, Some(10));
    assert_eq!(report.attempts, 1);
    assert!(report.checkpoint_entry_id.is_none());
    assert_eq!(
        repo.open(&session.metadata().id)
            .unwrap()
            .selected_branch()
            .model_context()
            .unwrap()
            .messages,
        before
    );
}

#[tokio::test]
async fn lineage_rejects_stale_equal_length_views_and_never_guesses_duplicate_ids() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = seed(&repo, root.path());
    repo.append(&mut s, Message::text(MessageRole::User, "duplicate"))
        .unwrap();
    repo.append(&mut s, Message::text(MessageRole::User, "duplicate"))
        .unwrap();
    let branch = s.selected_branch();
    let source = branch.model_context().unwrap().into_owned();
    let raw = branch.chat_messages();
    let positions = || s.messages().into_iter().enumerate().map(|(i, m)| (m.id, i));
    let projection =
        CompactionProjection::from_session_view(&source, &source.messages, 0, &raw, positions())
            .unwrap()
            .unwrap();
    assert_eq!(
        projection.canonical_index(raw.len() - 2),
        Some(raw.len() - 2)
    );
    assert_eq!(
        projection.canonical_index(raw.len() - 1),
        Some(raw.len() - 1)
    );
    assert!(
        CompactionProjection::from_session_view(
            &source,
            &source.messages[..source.messages.len() - 1],
            0,
            &raw,
            positions()
        )
        .unwrap()
        .is_none()
    );
    let mut unknown = source.clone();
    unknown.entry_ids[0] = Some("foreign-id".into());
    assert!(
        CompactionProjection::from_session_view(&unknown, &unknown.messages, 0, &raw, positions())
            .unwrap()
            .is_none()
    );
    let mut changed = source.messages.clone();
    changed.last_mut().unwrap().content = vec![ContentBlock::Text {
        text: "changed".into(),
    }];
    let provider = Script::new(&[]);
    let compactor = SemanticContextCompactor::new(provider.clone(), "test", config()).unwrap();
    let error = compactor
        .compact_projected(
            CompactionRequest {
                input: ContextCompactionInput {
                    model: "test",
                    messages: &changed,
                    tools: None,
                    max_output_tokens: None,
                },
                history: &raw,
                system_prefix_len: 0,
                force: true,
                token_budget: None,
                archived_messages: 0,
            },
            ContextMeasure {
                counting: Default::default(),
                spec: provider.model_spec("test").unwrap(),
                response_format: None,
                tool_choice: ModelToolChoice::None,
                target_tokens: 1000,
            },
            Some(&projection),
            TurnExecutionContext::new(),
        )
        .await
        .err()
        .unwrap();
    let failure = error.downcast_ref::<SummaryFailure>().unwrap();
    assert_eq!(failure.attempts, 0);
    assert_eq!(failure.usage, ModelUsage::zero());
    assert_eq!(provider.summaries(), 0);
    let mut changed_history = raw.clone();
    changed_history[0] = Message::text(MessageRole::User, "different canonical history");
    let error = compactor
        .compact_projected(
            CompactionRequest {
                input: ContextCompactionInput {
                    model: "test",
                    messages: &source.messages,
                    tools: None,
                    max_output_tokens: None,
                },
                history: &changed_history,
                system_prefix_len: 0,
                force: true,
                token_budget: None,
                archived_messages: 0,
            },
            ContextMeasure {
                counting: Default::default(),
                spec: provider.model_spec("test").unwrap(),
                response_format: None,
                tool_choice: ModelToolChoice::None,
                target_tokens: 1000,
            },
            Some(&projection),
            TurnExecutionContext::new(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.downcast_ref::<SummaryFailure>().unwrap().attempts, 0);
    assert_eq!(provider.summaries(), 0);
}
#[tokio::test]
async fn version_one_checkpoints_keep_their_original_message_only_counting_rules() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = seed(&repo, root.path());
    repo.append_entries(
        &mut s,
        vec![
            SessionEntryData::CustomMessage {
                custom: CustomMessage::text("source", "kind", "custom").unwrap(),
            },
            SessionEntryData::native_context(
                NativeContextSource {
                    extension: "source".into(),
                    hook: "hook".into(),
                    kind: "context".into(),
                },
                "native",
            )
            .unwrap(),
        ],
    )
    .unwrap();
    repo.append(&mut s, Message::text(MessageRole::User, "tail"))
        .unwrap();
    let mut c = config();
    c.window.preserve_recent_tokens = 1;
    let a = agent(Script::new(&[]), root.path(), c);
    assert!(
        a.compact_session(&repo, &s.metadata().id, TurnExecutionContext::new())
            .await
            .succeeded()
    );
    let saved = repo.open(&s.metadata().id).unwrap();
    let expected = saved.selected_branch().model_context().unwrap().messages;
    let mut entries = saved.entries().to_vec();
    let mut checkpoint = checkpoint(&saved);
    assert_eq!(checkpoint.summarized_messages, 11);
    checkpoint.version = 1;
    checkpoint.summarized_messages = 9;
    entries.last_mut().unwrap().data = checkpoint.data();
    let restored = Session::from_entries(saved.metadata().clone(), entries).unwrap();
    assert_eq!(
        restored.selected_branch().model_context().unwrap().messages,
        expected
    );
    for version in [2, 3] {
        let mut invalid = checkpoint.clone();
        invalid.version = version;
        assert!(
            validate_checkpoint(
                &invalid,
                &restored.selected_branch().entries()[..restored.entries().len() - 1],
                restored.entries().last().unwrap().parent_id.as_deref()
            )
            .is_err()
        );
    }
}
#[tokio::test]
async fn omitted_recovered_tool_units_still_mark_summary_uncertainty() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = repo.create(root.path()).unwrap();
    repo.append(
        &mut s,
        Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "uncertain".into(),
                    name: "read".into(),
                    arguments: json!({}),
                },
            }],
        },
    )
    .unwrap();
    let missing = s.metadata().leaf_id.clone().unwrap();
    for m in history().into_iter().skip(1) {
        repo.append(&mut s, m).unwrap();
    }
    context_edit(&repo, &s, &missing, ContextReplacement::Omit);
    let before = repo
        .open(&s.metadata().id)
        .unwrap()
        .selected_branch()
        .model_context()
        .unwrap()
        .messages;
    assert!(!before.iter().any(|m| m.role == MessageRole::Tool));
    let report = agent(Script::new(&[]), root.path(), config())
        .compact_session(&repo, &s.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(
        report.succeeded() && report.checkpoint_entry_id.is_some(),
        "{report:?}"
    );
    let saved = repo.open(&s.metadata().id).unwrap();
    assert!(checkpoint(&saved).draft.uncertain_operations);
    assert!(
        saved
            .selected_branch()
            .model_context()
            .unwrap()
            .messages
            .iter()
            .any(|m| m.role == MessageRole::System)
    );
}

#[derive(Clone, Copy)]
enum Intercept {
    Veto,
    Replace,
    Generated,
    GeneratedWait,
    Instructions,
    ReplaceInstructions,
    Invalid,
}
struct Interceptor {
    mode: Intercept,
    provider: Arc<Script>,
    events: Mutex<Vec<extensions::CompactionEvent>>,
    after_fail: bool,
    after_wait: bool,
}
#[async_trait]
impl extensions::CompactionHook for Interceptor {
    async fn before(
        &self,
        i: extensions::CompactionHookInput<'_>,
        _: extensions::ExtensionCancellation,
    ) -> anyhow::Result<Option<extensions::CompactionAction>> {
        use extensions::*;
        let mut action = CompactionAction::default();
        match self.mode {
            Intercept::Veto => action.cancel = true,
            Intercept::Instructions | Intercept::ReplaceInstructions => {
                action.instructions = Some(SummaryInstructions {
                    text: "HOST_PRIVATE_SUMMARY_GUIDANCE".into(),
                    replace_default: matches!(self.mode, Intercept::ReplaceInstructions),
                })
            }
            _ => {
                let summary =
                    if matches!(self.mode, Intercept::Generated | Intercept::GeneratedWait) {
                        let mut r = ModelRequest::new(
                            "test",
                            vec![
                                Message::text(
                                    MessageRole::System,
                                    if matches!(self.mode, Intercept::GeneratedWait) {
                                        "Summarize historical native data"
                                    } else {
                                        "Native replacement model"
                                    },
                                ),
                                Message::text(MessageRole::User, "public historical facts"),
                            ],
                        );
                        r.tool_choice = ModelToolChoice::None;
                        r.options.max_output_tokens = Some(64);
                        i.models
                            .generate(self.provider.clone(), r)
                            .await?
                            .message
                            .text_content()
                    } else {
                        "native replacement summary".into()
                    };
                let target = if matches!(self.mode, Intercept::Invalid) {
                    Some("missing".into())
                } else {
                    i.session_context
                        .and_then(|s| s.entry_ids.iter().rev().flatten().next().cloned())
                };
                action.replacement = Some(CompactionReplacement {
                    summary,
                    first_kept_entry_id: target,
                    details: Some(json!({"not_in_prompt":"PRIVATE_INERT_DETAILS"})),
                });
            }
        }
        Ok(Some(action))
    }
    async fn after(
        &self,
        e: &extensions::CompactionEvent,
        _: extensions::ExtensionCancellation,
    ) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(e.clone());
        if self.after_wait {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
        anyhow::ensure!(!self.after_fail, "PRIVATE_OBSERVER_ERROR");
        Ok(())
    }
}
fn intercept_agent(
    root: &std::path::Path,
    provider: Arc<Script>,
    hook: Arc<Interceptor>,
    duration: Option<std::time::Duration>,
) -> Agent {
    use extensions::*;
    let mut e = Extension::new("compact", "1").unwrap();
    e.compaction_hook(HookSpec::new("native"), hook).unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let mut c = config();
    c.failure_policy = SummaryFailurePolicy::Stop;
    Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root)
            .extensions(Arc::new(r.finish()))
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_duration: duration,
                    ..Default::default()
                },
                ..Default::default()
            })
            .context_compactor(SemanticContextCompactor::new(provider, "test", c).unwrap())
            .build()
            .unwrap(),
        AgentConfig::new("test").system_prompt("trusted prefix"),
    )
}
fn interceptor(mode: Intercept, p: Arc<Script>) -> Arc<Interceptor> {
    Arc::new(Interceptor {
        mode,
        provider: p,
        events: Mutex::new(vec![]),
        after_fail: false,
        after_wait: false,
    })
}
#[tokio::test]
async fn compaction_veto_stops_auto_io_and_reports_manual_cancel_without_cancelling_host() {
    for manual in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let s = seed(&repo, root.path());
        let p = Script::new(&[]);
        let h = interceptor(Intercept::Veto, p.clone());
        let a = intercept_agent(root.path(), p.clone(), h.clone(), None);
        let context = TurnExecutionContext::new();
        if manual {
            let report = a
                .compact_session(&repo, &s.metadata().id, context.clone())
                .await;
            assert!(!report.succeeded());
            assert!(matches!(report.stop, Some(RunStop::Cancelled { .. })));
            assert!(report.checkpoint_entry_id.is_none());
            assert_eq!(report.attempts, 0);
        } else {
            let out = a
                .run_with_session_outcome(
                    &repo,
                    SessionSelection::Existing(s.metadata().id.clone()),
                    "fresh",
                    Arc::new(()),
                    context.clone(),
                )
                .await;
            assert!(!matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
            assert_eq!(out.progress().usage, ModelUsage::zero());
        }
        assert!(p.requests.lock().unwrap().is_empty());
        assert!(!context.is_cancelled());
        assert_eq!(
            h.events.lock().unwrap()[0].status,
            extensions::CompactionStatus::Vetoed
        );
    }
}
#[tokio::test]
async fn native_replacements_commit_before_auto_main_io_and_preserve_manual_history() {
    for manual in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let s = seed(&repo, root.path());
        let raw = s.selected_branch().chat_messages();
        let p = Script::new(&[]);
        let h = interceptor(Intercept::Replace, p.clone());
        let a = intercept_agent(root.path(), p.clone(), h.clone(), None);
        if manual {
            let report = a
                .compact_session(&repo, &s.metadata().id, TurnExecutionContext::new())
                .await;
            assert!(
                report.succeeded() && report.checkpoint_entry_id.is_some(),
                "{report:?}"
            );
            assert_eq!(report.attempts, 0);
            assert_eq!(report.usage, ModelUsage::zero());
            assert_eq!(
                repo.open(&s.metadata().id)
                    .unwrap()
                    .selected_branch()
                    .chat_messages(),
                raw
            );
        } else {
            let out = a
                .run_with_session_outcome(
                    &repo,
                    SessionSelection::Existing(s.metadata().id.clone()),
                    "fresh",
                    Arc::new(()),
                    TurnExecutionContext::new(),
                )
                .await;
            assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
            assert_eq!(out.progress().usage.total_tokens, Some(10));
            assert_eq!(p.requests.lock().unwrap().len(), 1);
            let r = &p.requests.lock().unwrap()[0];
            assert!(
                r.messages
                    .iter()
                    .any(|m| m.text_content().contains("native replacement summary"))
            );
            assert!(!serde_json::to_string(r).unwrap().contains("PRIVATE_INERT"));
        }
        let event = h.events.lock().unwrap()[0].clone();
        assert_eq!(event.status, extensions::CompactionStatus::Completed);
        assert!(event.from_extension && event.checkpoint_entry_id.is_some());
        assert_eq!(p.summaries(), 0);
    }
}
#[tokio::test]
async fn native_replacement_model_work_is_charged_separately_for_manual_and_auto() {
    for manual in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let s = seed(&repo, root.path());
        let p = Script::new(&[]);
        let h = interceptor(Intercept::Generated, p.clone());
        let a = intercept_agent(root.path(), p.clone(), h, None);
        if manual {
            let report = a
                .compact_session(&repo, &s.metadata().id, TurnExecutionContext::new())
                .await;
            assert!(report.succeeded(), "{report:?}");
            assert_eq!(report.usage.total_tokens, Some(10));
            assert_eq!(report.primary_usage.unwrap().total_tokens, Some(0));
            assert_eq!(report.auxiliary.len(), 1);
            assert_eq!(report.branch_usage.unwrap().total_tokens, Some(10));
            assert_eq!(report.attempts, 0);
        } else {
            let out = a
                .run_with_session_outcome(
                    &repo,
                    SessionSelection::Existing(s.metadata().id.clone()),
                    "fresh",
                    Arc::new(()),
                    TurnExecutionContext::new(),
                )
                .await;
            assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
            assert_eq!(out.progress().usage.total_tokens, Some(20));
            assert_eq!(
                out.progress().primary_usage.as_ref().unwrap().total_tokens,
                Some(10)
            );
            assert_eq!(out.progress().auxiliary.len(), 1);
        }
        assert_eq!(p.summaries(), 0);
        assert!(
            repo.open(&s.metadata().id)
                .unwrap()
                .selected_branch()
                .accounting()
                .unwrap()
                .usage
                .total_tokens
                .unwrap()
                >= 10
        );
    }
}
#[tokio::test]
async fn native_instructions_customize_default_requests_without_becoming_history() {
    for mode in [Intercept::Instructions, Intercept::ReplaceInstructions] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let s = seed(&repo, root.path());
        let p = Script::new(&[]);
        let h = interceptor(mode, p.clone());
        let a = intercept_agent(root.path(), p.clone(), h, None);
        let report = a
            .compact_session(&repo, &s.metadata().id, TurnExecutionContext::new())
            .await;
        assert!(
            report.succeeded() && report.checkpoint_entry_id.is_some(),
            "{report:?}"
        );
        let requests = p.requests.lock().unwrap();
        assert!(!requests.is_empty());
        for r in requests.iter() {
            assert!(
                r.messages[0]
                    .text_content()
                    .contains("HOST_PRIVATE_SUMMARY_GUIDANCE")
            );
            assert!(r.messages[0].text_content().contains("five arrays"));
            assert!(r.tools.is_empty());
        }
        assert!(
            !serde_json::to_string(repo.open(&s.metadata().id).unwrap().entries())
                .unwrap()
                .contains("HOST_PRIVATE_SUMMARY_GUIDANCE")
        );
    }
}
#[tokio::test]
async fn invalid_native_replacement_never_falls_back_or_mutates_canonical_facts() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let raw = s.selected_branch().chat_messages();
    let p = Script::new(&[]);
    let h = interceptor(Intercept::Invalid, p.clone());
    let a = intercept_agent(root.path(), p.clone(), h.clone(), None);
    let report = a
        .compact_session(&repo, &s.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(!report.succeeded());
    assert!(p.requests.lock().unwrap().is_empty());
    assert_eq!(
        repo.open(&s.metadata().id)
            .unwrap()
            .selected_branch()
            .chat_messages(),
        raw
    );
    assert_eq!(
        h.events.lock().unwrap()[0].status,
        extensions::CompactionStatus::Failed
    );
}
#[tokio::test]
async fn post_compaction_observer_errors_cannot_veto_the_acknowledged_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let p = Script::new(&[]);
    let h = Arc::new(Interceptor {
        mode: Intercept::Generated,
        provider: p.clone(),
        events: Mutex::new(vec![]),
        after_fail: true,
        after_wait: false,
    });
    let a = intercept_agent(root.path(), p.clone(), h, None);
    let report = a
        .compact_session(&repo, &s.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(
        report.succeeded() && report.checkpoint_entry_id.is_some(),
        "{report:?}"
    );
    assert_eq!(report.delivery.failures.len(), 1);
    assert_eq!(report.usage.total_tokens, Some(10));
    assert_eq!(p.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn observer_deadline_retains_manual_checkpoint_and_paid_usage() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let p = Script::new(&[]);
    let h = Arc::new(Interceptor {
        mode: Intercept::Generated,
        provider: p.clone(),
        events: Mutex::new(vec![]),
        after_fail: false,
        after_wait: true,
    });
    let a = intercept_agent(
        root.path(),
        p.clone(),
        h,
        Some(std::time::Duration::from_millis(80)),
    );
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        a.compact_session(&repo, &s.metadata().id, TurnExecutionContext::new()),
    )
    .await
    .unwrap();
    assert!(report.checkpoint_entry_id.is_some(), "{report:?}");
    assert!(matches!(report.stop, Some(RunStop::Budget(_))));
    assert_eq!(report.usage.total_tokens, Some(10));
    assert_eq!(report.delivery.completed, 0);
    assert_eq!(p.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancelled_manual_hook_settles_partial_native_receipts_without_a_fake_turn() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let p = Script::new(&[Reply::Wait]);
    let h = interceptor(Intercept::GeneratedWait, p.clone());
    let a = intercept_agent(
        root.path(),
        p.clone(),
        h,
        Some(std::time::Duration::from_millis(40)),
    );
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        a.compact_session(&repo, &s.metadata().id, TurnExecutionContext::new()),
    )
    .await
    .unwrap();
    assert!(!report.succeeded(), "{report:?}");
    assert_eq!(report.usage.total_tokens, Some(10));
    assert_eq!(report.branch_usage.as_ref().unwrap().total_tokens, Some(10));
    assert_eq!(report.auxiliary.len(), 1);
    assert_eq!(report.auxiliary[0].status, "interrupted");
    assert!(report.checkpoint_entry_id.is_none());
    let stored = repo.open(&s.metadata().id).unwrap();
    assert!(!stored.entries().iter().any(|e| matches!(
        e.data,
        SessionEntryData::TurnStart { .. } | SessionEntryData::TurnEnd { .. }
    )));
    assert_eq!(stored.messages().len(), 9);
}
#[tokio::test]
async fn customized_instruction_failures_do_not_poison_another_instruction_cache_key() {
    let p = Script::new(&[Reply::Fail]);
    let mut c = config();
    c.max_attempts = 1;
    let compactor = SemanticContextCompactor::new(p.clone(), "test", c).unwrap();
    let messages = history();
    let request = || CompactionRequest {
        input: ContextCompactionInput {
            model: "test",
            messages: &messages,
            tools: None,
            max_output_tokens: None,
        },
        history: &messages,
        system_prefix_len: 1,
        force: false,
        token_budget: None,
        archived_messages: 0,
    };
    let measure = || ContextMeasure {
        counting: Default::default(),
        spec: p.model_spec("test").unwrap(),
        response_format: None,
        tool_choice: ModelToolChoice::None,
        target_tokens: 1000,
    };
    let first = extensions::SummaryInstructions {
        text: "first host guidance".into(),
        replace_default: false,
    };
    let result = compactor
        .compact_customized(
            request(),
            measure(),
            None,
            Some(&first),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert!(result.checkpoint.is_none());
    let calls = p.summaries();
    let result = compactor
        .compact_customized(
            request(),
            measure(),
            None,
            Some(&first),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert!(result.checkpoint.is_none());
    assert_eq!(p.summaries(), calls);
    let other = extensions::SummaryInstructions {
        text: "different host guidance".into(),
        replace_default: false,
    };
    assert!(
        compactor
            .compact_customized(
                request(),
                measure(),
                None,
                Some(&other),
                TurnExecutionContext::new()
            )
            .await
            .unwrap()
            .checkpoint
            .is_some()
    );
    assert!(p.summaries() > calls);
}

struct DefaultAfterReplacement;
#[async_trait]
impl extensions::CompactionHook for DefaultAfterReplacement {
    async fn before(
        &self,
        i: extensions::CompactionHookInput<'_>,
        _: extensions::ExtensionCancellation,
    ) -> anyhow::Result<Option<extensions::CompactionAction>> {
        assert!(i.replacement.is_some());
        Ok(Some(extensions::CompactionAction {
            use_default: true,
            instructions: Some(extensions::SummaryInstructions {
                text: "ROOT_PRIVATE_LATER_GUIDANCE".into(),
                replace_default: false,
            }),
            ..Default::default()
        }))
    }
}
#[tokio::test]
async fn later_hook_can_restore_default_generation_without_erasing_earlier_native_usage() {
    use extensions::*;
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let p = Script::new(&[]);
    let mut e = Extension::new("chain", "1").unwrap();
    e.compaction_hook(
        HookSpec::new("first"),
        interceptor(Intercept::Generated, p.clone()),
    )
    .unwrap();
    let mut later = HookSpec::new("later");
    later.priority = 10;
    e.compaction_hook(later, Arc::new(DefaultAfterReplacement))
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let a = Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .context_compactor(SemanticContextCompactor::new(p.clone(), "test", config()).unwrap())
            .build()
            .unwrap(),
        AgentConfig::new("test"),
    );
    let report = a
        .compact_session(&repo, &s.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(
        report.succeeded() && report.checkpoint_entry_id.is_some(),
        "{report:?}"
    );
    assert_eq!(report.auxiliary.len(), 1);
    assert!(report.attempts > 0);
    assert_eq!(
        report.usage.total_tokens,
        Some((report.attempts as u64 + 1) * 10)
    );
    assert_eq!(
        report.branch_usage.unwrap().total_tokens,
        report.usage.total_tokens
    );
    assert!(
        !serde_json::to_string(repo.open(&s.metadata().id).unwrap().entries())
            .unwrap()
            .contains("ROOT_PRIVATE_LATER_GUIDANCE")
    );
}
