use aicoder_core::{
    completion::{CompletionContext, CompletionVerdict, CompletionVerifier},
    model::*,
    tools::*,
    *,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone)]
enum Step {
    Reply(ModelResponse),
    Fail,
    Retry(ModelResponse),
    Pending,
    Stream,
}
struct Script {
    steps: Mutex<VecDeque<Step>>,
    requests: Mutex<Vec<ModelRequest>>,
    started: Arc<tokio::sync::Notify>,
}
impl Script {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps.into()),
            requests: Mutex::new(Vec::new()),
            started: Arc::new(tokio::sync::Notify::new()),
        })
    }
}
#[async_trait]
impl ModelProvider for Script {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("test", model);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _context: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.requests.lock().unwrap().push(request);
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted");
        match step {
            Step::Reply(response) => {
                emit_response(&response, sink.as_ref());
                Ok(response)
            }
            Step::Retry(response) => {
                sink.emit(ModelEvent::Retry {
                    attempt: 2,
                    delay_ms: 1,
                    reason: "retry".into(),
                });
                emit_response(&response, sink.as_ref());
                Ok(response)
            }
            Step::Fail => Err(ProviderError {
                http_status: Some(401),
                ..ProviderError::new("test", ProviderErrorKind::Authentication, "bad credentials")
            }),
            Step::Pending => {
                self.started.notify_one();
                std::future::pending().await
            }
            Step::Stream => {
                sink.emit(ModelEvent::Started);
                sink.emit(ModelEvent::TextDelta {
                    delta: "你好abc".into(),
                });
                sink.emit(ModelEvent::Usage {
                    usage: ModelUsage {
                        total_tokens: Some(7),
                        ..Default::default()
                    },
                });
                self.started.notify_one();
                std::future::pending().await
            }
        }
    }
}
fn reply(text: &str) -> ModelResponse {
    ModelResponse {
        id: None,
        model: "test".into(),
        message: Message::text(MessageRole::Assistant, text),
        stop_reason: StopReason::Complete,
        usage: ModelUsage::default(),
    }
}
fn calls(names: &[&str]) -> ModelResponse {
    let mut response = reply("");
    response.stop_reason = StopReason::ToolCalls;
    response.message.content = names
        .iter()
        .enumerate()
        .map(|(i, name)| ContentBlock::ToolCall {
            call: ModelToolCall {
                id: format!("c{i}"),
                name: (*name).into(),
                arguments: json!({}),
            },
        })
        .collect();
    response
}
fn request() -> ModelRequest {
    ModelRequest::new("test", vec![Message::text(MessageRole::User, "work")])
}
fn config(budget: RunBudget) -> TurnExecutionConfig {
    TurnExecutionConfig {
        budget,
        ..Default::default()
    }
}
fn executor(provider: Arc<Script>, budget: RunBudget) -> TurnExecutor {
    TurnExecutor::builder_from_shared(provider)
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .config(config(budget))
        .build()
        .unwrap()
}
async fn run(executor: &TurnExecutor) -> RunOutcome {
    executor
        .run_outcome(request(), Arc::new(()), TurnExecutionContext::new())
        .await
}
fn stopped(outcome: RunOutcome) -> InterruptedTurn {
    match outcome {
        RunOutcome::Interrupted(turn) => turn,
        other => panic!("unexpected {other:?}"),
    }
}
fn limit(turn: &InterruptedTurn) -> BudgetResource {
    match &turn.stop {
        RunStop::Budget(limit) => limit.resource,
        other => panic!("unexpected {other:?}"),
    }
}
struct Tool {
    name: &'static str,
    pending: bool,
    size: usize,
    done: Arc<AtomicUsize>,
    notify: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl ExecutableTool for Tool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        if self.pending {
            return std::future::pending().await;
        }
        self.done.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_one();
        Ok(ToolSuccess {
            output: json!("x".repeat(self.size)),
            truncated: false,
        })
    }
}
fn tools(
    names: &[(&'static str, bool, usize)],
    done: &Arc<AtomicUsize>,
    notify: &Arc<tokio::sync::Notify>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::default();
    for (name, pending, size) in names {
        registry
            .register(Tool {
                name,
                pending: *pending,
                size: *size,
                done: done.clone(),
                notify: notify.clone(),
            })
            .unwrap();
    }
    registry
}
#[tokio::test]
async fn cancellation_retains_partial_stream_and_reported_usage() {
    let provider = Script::new(vec![Step::Stream]);
    let started = provider.started.clone();
    let executor = executor(provider, RunBudget::default());
    let context = TurnExecutionContext::new();
    let cancel = context.clone();
    let run =
        tokio::spawn(async move { executor.run_outcome(request(), Arc::new(()), context).await });
    started.notified().await;
    cancel.cancel("user stop");
    let turn = stopped(run.await.unwrap());
    assert!(matches!(turn.stop, RunStop::Cancelled { .. }));
    assert_eq!(turn.usage.total_tokens, Some(7));
    assert_eq!(turn.partial_response.as_ref().unwrap().text, "你好abc");
    assert_eq!(turn.messages.len(), 1);
}
#[tokio::test]
async fn cancellation_preserves_completed_results_in_a_parallel_tool_batch() {
    let provider = Script::new(vec![Step::Reply(calls(&["fast", "slow"]))]);
    let done = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder_from_shared(provider)
        .workspace(std::env::temp_dir())
        .registry(tools(
            &[("fast", false, 4), ("slow", true, 0)],
            &done,
            &notify,
        ))
        .build()
        .unwrap();
    let context = TurnExecutionContext::new();
    let cancel = context.clone();
    let run =
        tokio::spawn(async move { executor.run_outcome(request(), Arc::new(()), context).await });
    notify.notified().await;
    cancel.cancel("stop batch");
    let turn = stopped(run.await.unwrap());
    assert_eq!(turn.messages.len(), 3);
    assert_eq!(turn.messages[2].tool_result().unwrap().0, "c0");
    assert_eq!(turn.tool_calls, 2);
    assert_eq!(done.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn round_limit_returns_complete_tool_history_and_error_wrapper_keeps_progress() {
    let provider = Script::new(vec![Step::Reply(calls(&["fast"]))]);
    let done = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder_from_shared(provider)
        .workspace(std::env::temp_dir())
        .registry(tools(&[("fast", false, 4)], &done, &notify))
        .config(TurnExecutionConfig {
            max_rounds: 1,
            ..Default::default()
        })
        .build()
        .unwrap();
    let error = executor.run(request()).await.unwrap_err();
    let turn = error.downcast_ref::<InterruptedTurn>().unwrap();
    assert_eq!(limit(turn), BudgetResource::Rounds);
    assert_eq!(turn.messages.len(), 3);
    assert_eq!(turn.tool_calls, 1);
    assert!(error.downcast_ref::<BudgetExceeded>().is_some());
}
#[tokio::test]
async fn global_tool_budget_rejects_a_batch_before_any_tool_starts() {
    let provider = Script::new(vec![Step::Reply(calls(&["a", "b"]))]);
    let done = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder_from_shared(provider)
        .workspace(std::env::temp_dir())
        .registry(tools(&[("a", false, 1), ("b", false, 1)], &done, &notify))
        .config(config(RunBudget {
            max_tool_calls: Some(1),
            ..Default::default()
        }))
        .build()
        .unwrap();
    let turn = stopped(run(&executor).await);
    assert_eq!(limit(&turn), BudgetResource::ToolCalls);
    assert_eq!(done.load(Ordering::SeqCst), 0);
    assert_eq!(turn.messages.len(), 2);
}
#[tokio::test]
async fn output_limit_is_utf8_safe_and_exact_fit_can_complete() {
    for (maximum, complete) in [(5, false), (6, true)] {
        let provider = Script::new(vec![Step::Reply(reply("你好"))]);
        let context = TurnExecutionContext::new();
        let executor = executor(
            provider,
            RunBudget {
                max_output_bytes: Some(maximum),
                ..Default::default()
            },
        );
        let outcome = executor
            .run_outcome(request(), Arc::new(()), context.clone())
            .await;
        if complete {
            assert!(matches!(outcome, RunOutcome::Completed(_)));
            assert_eq!(outcome.progress().output_bytes, 6);
        } else {
            let turn = stopped(outcome);
            assert_eq!(limit(&turn), BudgetResource::OutputBytes);
            assert_eq!(turn.partial_response.as_ref().unwrap().text, "你");
            assert_eq!(turn.progress.output_bytes, 3);
        }
        assert!(!context.is_cancelled());
    }
}
#[tokio::test]
async fn large_tool_output_is_truncated_but_the_completed_operation_is_retained() {
    let provider = Script::new(vec![Step::Reply(calls(&["fast"]))]);
    let done = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(std::env::temp_dir())
        .registry(tools(&[("fast", false, 200)], &done, &notify))
        .config(config(RunBudget {
            max_output_bytes: Some(20),
            ..Default::default()
        }))
        .build()
        .unwrap();
    let turn = stopped(run(&executor).await);
    assert_eq!(limit(&turn), BudgetResource::OutputBytes);
    assert_eq!(done.load(Ordering::SeqCst), 1);
    assert_eq!(turn.messages.len(), 3);
    assert_eq!(turn.messages[2].tool_result().unwrap().1["truncated"], true);
    assert!(turn.output_bytes <= 20);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}
#[tokio::test(start_paused = true)]
async fn deadline_cancels_an_uncooperative_provider_without_cancelling_the_caller() {
    let provider = Script::new(vec![Step::Pending]);
    let executor = executor(
        provider,
        RunBudget {
            max_duration: Some(Duration::from_millis(10)),
            ..Default::default()
        },
    );
    let context = TurnExecutionContext::new();
    let turn = stopped(
        executor
            .run_outcome(request(), Arc::new(()), context.clone())
            .await,
    );
    assert_eq!(limit(&turn), BudgetResource::Duration);
    assert!(turn.elapsed >= Duration::from_millis(10));
    assert!(!context.is_cancelled());
    assert_eq!(turn.rounds, 1);
}
struct PendingVerifier;
#[async_trait]
impl CompletionVerifier for PendingVerifier {
    async fn verify(&self, _: CompletionContext<'_>) -> anyhow::Result<CompletionVerdict> {
        std::future::pending().await
    }
}
#[tokio::test(start_paused = true)]
async fn deadline_covers_verification_and_keeps_the_candidate() {
    let provider = Script::new(vec![Step::Reply(reply("candidate"))]);
    let executor = TurnExecutor::builder_from_shared(provider)
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .completion_verifier(PendingVerifier)
        .config(config(RunBudget {
            max_duration: Some(Duration::from_millis(10)),
            ..Default::default()
        }))
        .build()
        .unwrap();
    let turn = stopped(run(&executor).await);
    assert_eq!(limit(&turn), BudgetResource::Duration);
    assert_eq!(turn.stage, RunStage::Verification);
    assert_eq!(turn.messages.last().unwrap().text_content(), "candidate");
}
#[tokio::test]
async fn token_preflight_avoids_a_request_and_reported_overrun_retains_the_answer() {
    let provider = Script::new(Vec::new());
    let executor = executor(
        provider.clone(),
        RunBudget {
            max_tokens: Some(1),
            ..Default::default()
        },
    );
    let turn = stopped(run(&executor).await);
    assert_eq!(limit(&turn), BudgetResource::Tokens);
    assert!(provider.requests.lock().unwrap().is_empty());
    let mut response = reply("answer");
    response.usage = ModelUsage {
        total_tokens: Some(1001),
        ..Default::default()
    };
    let provider = Script::new(vec![Step::Reply(response)]);
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .config(config(RunBudget {
            max_tokens: Some(1000),
            ..Default::default()
        }))
        .build()
        .unwrap();
    let turn = stopped(run(&executor).await);
    assert_eq!(limit(&turn), BudgetResource::Tokens);
    assert_eq!(turn.messages.last().unwrap().text_content(), "answer");
    assert_eq!(turn.usage.total_tokens, Some(1001));
    assert!(!turn.tokens.estimated);
    assert!(
        provider.requests.lock().unwrap()[0]
            .options
            .max_output_tokens
            .unwrap()
            < 1000
    );
}
#[tokio::test]
async fn unknown_usage_is_estimated_only_for_budget_accounting_and_model_stop_is_explicit() {
    let mut response = reply("partial");
    response.stop_reason = StopReason::MaxTokens;
    let provider = Script::new(vec![Step::Reply(response)]);
    let turn = stopped(run(&executor(provider, RunBudget::default())).await);
    assert!(matches!(turn.stop, RunStop::Model(StopReason::MaxTokens)));
    assert_eq!(turn.usage.total_tokens, None);
    assert!(turn.tokens.estimated);
    assert!(turn.tokens.used > 0);
    assert_eq!(turn.messages.last().unwrap().text_content(), "partial");
}
#[tokio::test]
async fn typed_provider_failure_retains_earlier_tool_interactions() {
    let provider = Script::new(vec![Step::Reply(calls(&["fast"])), Step::Fail]);
    let done = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder_from_shared(provider)
        .workspace(std::env::temp_dir())
        .registry(tools(&[("fast", false, 1)], &done, &notify))
        .build()
        .unwrap();
    let turn = stopped(run(&executor).await);
    assert_eq!(turn.messages.len(), 3);
    assert_eq!(turn.rounds, 2);
    match &turn.stop {
        RunStop::Failed(error) => {
            assert_eq!(error.stage, RunStage::Model);
            assert_eq!(
                error.provider.as_ref().unwrap().kind,
                ProviderErrorKind::Authentication
            );
        }
        other => panic!("unexpected {other:?}"),
    };
    let encoded = serde_json::to_string(&turn).unwrap();
    let decoded: InterruptedTurn = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.messages, turn.messages);
}

struct PendingCompactor;
#[async_trait]
impl ContextCompactor for PendingCompactor {
    fn name(&self) -> &'static str {
        "pending"
    }
    fn estimate_tokens(&self, _: ContextCompactionInput<'_>) -> usize {
        100
    }
    fn target_tokens(&self, _: ContextCompactionInput<'_>) -> usize {
        10
    }
    async fn compact(&self, _: ContextCompactionInput<'_>) -> anyhow::Result<ContextCompaction> {
        std::future::pending().await
    }
}
#[tokio::test(start_paused = true)]
async fn deadline_covers_compaction_and_does_not_invent_zero_usage() {
    let provider = Script::new(Vec::new());
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .context_compactor(PendingCompactor)
        .config(config(RunBudget {
            max_duration: Some(Duration::from_millis(10)),
            ..Default::default()
        }))
        .build()
        .unwrap();
    let turn = stopped(run(&executor).await);
    assert_eq!(limit(&turn), BudgetResource::Duration);
    assert_eq!(turn.stage, RunStage::Compaction);
    assert_eq!(turn.usage.total_tokens, None);
    assert!(turn.tokens.estimated);
    assert!(provider.requests.lock().unwrap().is_empty());
}
struct PendingApproval;
#[async_trait]
impl ApprovalHandler for PendingApproval {
    async fn approve(&self, _: &ToolInvocation) -> anyhow::Result<bool> {
        std::future::pending().await
    }
}
struct ProtectedTool(Arc<AtomicUsize>);
#[async_trait]
impl ExecutableTool for ProtectedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "protected".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess {
            output: json!("done"),
            truncated: false,
        })
    }
}
#[tokio::test(start_paused = true)]
async fn deadline_closes_pending_approval_without_executing_a_mutation() {
    let provider = Script::new(vec![Step::Reply(calls(&["protected"]))]);
    let done = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry.register(ProtectedTool(done.clone())).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let executor = TurnExecutor::builder_from_shared(provider)
        .workspace(std::env::temp_dir())
        .registry(registry)
        .approval(PendingApproval)
        .config(config(RunBudget {
            max_duration: Some(Duration::from_millis(10)),
            ..Default::default()
        }))
        .build()
        .unwrap();
    let turn = stopped(
        executor
            .run_outcome(
                request(),
                Arc::new(move |e: &AgentRawEventEnvelope| {
                    captured.lock().unwrap().push(e.event.clone());
                }),
                TurnExecutionContext::new(),
            )
            .await,
    );
    assert_eq!(limit(&turn), BudgetResource::Duration);
    assert_eq!(done.load(Ordering::SeqCst), 0);
    assert_eq!(turn.tool_calls, 1);
    let events = events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRawEvent::ToolApprovalResolved {
            approved: false,
            ..
        }
    )));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentRawEvent::AgentStopped { .. }))
            .count(),
        1
    );
}

struct BoundedProfile(Arc<Script>);
#[async_trait]
impl ModelProvider for BoundedProfile {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = self.0.model_spec(model)?;
        spec.max_output_tokens = Some(64);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        context: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.0.generate(request, sink, context).await
    }
}
#[tokio::test]
async fn model_output_capacity_is_respected_without_mislabeling_it_as_task_quota() {
    let mut response = reply("partial");
    response.stop_reason = StopReason::MaxTokens;
    let provider = Script::new(vec![Step::Reply(response)]);
    let executor = TurnExecutor::builder(BoundedProfile(provider.clone()))
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .config(config(RunBudget {
            max_tokens: Some(1000),
            ..Default::default()
        }))
        .build()
        .unwrap();
    let turn = stopped(run(&executor).await);
    assert!(matches!(turn.stop, RunStop::Model(StopReason::MaxTokens)));
    assert_eq!(
        provider.requests.lock().unwrap()[0]
            .options
            .max_output_tokens,
        Some(64)
    );
}
#[tokio::test]
async fn retry_cost_is_estimated_and_does_not_masquerade_as_fully_reported_usage() {
    let mut response = reply("done");
    response.usage = ModelUsage {
        total_tokens: Some(5),
        ..Default::default()
    };
    let provider = Script::new(vec![Step::Retry(response)]);
    let executor = executor(
        provider,
        RunBudget {
            max_tokens: Some(1000),
            ..Default::default()
        },
    );
    let mut request = request();
    request.options.max_output_tokens = Some(32);
    let outcome = executor
        .run_outcome(request, Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert!(outcome.progress().reported_usage);
    assert_eq!(outcome.progress().usage.total_tokens, None);
    assert!(outcome.progress().tokens.estimated);
    assert!(outcome.progress().tokens.used > 5);
}

#[tokio::test]
async fn deterministic_pruning_can_make_a_request_fit_the_token_budget_without_model_cost() {
    let provider = Script::new(vec![Step::Reply(reply("done"))]);
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .context_compactor(
            PruningContextCompactor::new(ContextWindowConfig {
                max_context_tokens: 200,
                reserved_output_tokens: 100,
                preserve_recent_tokens: 20,
            })
            .unwrap(),
        )
        .config(config(RunBudget {
            max_tokens: Some(200),
            ..Default::default()
        }))
        .build()
        .unwrap();
    let mut request = request();
    request
        .messages
        .insert(0, Message::text(MessageRole::User, "x".repeat(4000)));
    let outcome = executor
        .run_outcome(request, Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert!(outcome.progress().tokens.used < 200);
    assert_eq!(outcome.progress().messages[0].text_content().len(), 4000);
}

#[tokio::test]
async fn output_retention_stays_bounded_after_another_budget_stops_the_run() {
    let provider = Script::new(vec![Step::Retry(reply(&"x".repeat(300)))]);
    let executor = executor(
        provider,
        RunBudget {
            max_tokens: Some(80),
            max_output_bytes: Some(5),
            ..Default::default()
        },
    );
    let mut request = request();
    request.options.max_output_tokens = Some(32);
    let turn = stopped(
        executor
            .run_outcome(request, Arc::new(()), TurnExecutionContext::new())
            .await,
    );
    assert_eq!(limit(&turn), BudgetResource::Tokens);
    assert!(turn.output_bytes <= 5);
    assert_eq!(turn.messages.len(), 1);
    assert!(turn.partial_response.as_ref().unwrap().text.len() <= 5);
}

#[tokio::test]
async fn rejected_model_tool_policy_retains_the_candidate_without_executing_it() {
    let provider = Script::new(vec![Step::Reply(calls(&["fast"]))]);
    let done = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder_from_shared(provider)
        .workspace(std::env::temp_dir())
        .registry(tools(&[("fast", false, 1)], &done, &notify))
        .build()
        .unwrap();
    let mut request = request();
    request.tool_choice = ModelToolChoice::None;
    let turn = stopped(
        executor
            .run_outcome(request, Arc::new(()), TurnExecutionContext::new())
            .await,
    );
    assert!(matches!(turn.stop, RunStop::Failed(_)));
    assert_eq!(turn.messages.len(), 2);
    assert!(turn.messages[1].tool_calls().next().is_some());
    assert_eq!(done.load(Ordering::SeqCst), 0);
}

struct FailingCompactor;
#[async_trait]
impl ContextCompactor for FailingCompactor {
    fn name(&self) -> &'static str {
        "failing"
    }
    fn estimate_tokens(&self, _: ContextCompactionInput<'_>) -> usize {
        100
    }
    fn target_tokens(&self, _: ContextCompactionInput<'_>) -> usize {
        10
    }
    async fn compact(&self, _: ContextCompactionInput<'_>) -> anyhow::Result<ContextCompaction> {
        Err(ProviderError::new(
            "summary",
            ProviderErrorKind::Authentication,
            "summary authentication failed",
        )
        .into())
    }
}
#[tokio::test]
async fn conservative_failure_accounting_does_not_replace_the_original_failure_with_a_quota() {
    let provider = Script::new(Vec::new());
    let executor = TurnExecutor::builder_from_shared(provider)
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .context_compactor(FailingCompactor)
        .config(config(RunBudget {
            max_tokens: Some(105),
            ..Default::default()
        }))
        .build()
        .unwrap();
    let turn = stopped(run(&executor).await);
    match &turn.stop {
        RunStop::Failed(error) => {
            assert_eq!(error.stage, RunStage::Compaction);
            assert_eq!(
                error.provider.as_ref().unwrap().kind,
                ProviderErrorKind::Authentication
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(turn.usage.total_tokens, None);
    assert!(turn.tokens.estimated);
    assert!(turn.tokens.used > 105);
}

struct FailingJournal {
    fail_tools: bool,
    checkpoints: Mutex<Vec<(usize, Message)>>,
    finished: AtomicUsize,
}
#[async_trait]
impl RunJournal for FailingJournal {
    async fn checkpoint(&self, index: usize, message: &Message) -> anyhow::Result<()> {
        if message.role == MessageRole::Tool && self.fail_tools {
            anyhow::bail!("disk full");
        }
        self.checkpoints
            .lock()
            .unwrap()
            .push((index, message.clone()));
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> anyhow::Result<()> {
        self.finished.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("terminal write failed")
    }
}
#[tokio::test]
async fn journal_failure_stops_execution_retains_completed_operations_and_reports_secondary_failure()
 {
    let script = Script::new(vec![
        Step::Reply(calls(&["done", "pending"])),
        Step::Reply(reply("must not run")),
    ]);
    let done = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let executor = TurnExecutor::builder_from_shared(script.clone())
        .workspace(std::env::temp_dir())
        .registry(tools(
            &[("done", false, 4), ("pending", true, 0)],
            &done,
            &notify,
        ))
        .build()
        .unwrap();
    let journal = FailingJournal {
        fail_tools: true,
        checkpoints: Mutex::new(vec![]),
        finished: AtomicUsize::new(0),
    };
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        executor.run_with_journal(
            request(),
            Arc::new(()),
            TurnExecutionContext::new(),
            &journal,
        ),
    )
    .await
    .unwrap();
    let turn = stopped(outcome);
    assert!(matches!(&turn.stop,RunStop::Failed(failure) if failure.stage==RunStage::Persistence));
    assert!(turn.persistence_error.is_some());
    assert_eq!(done.load(Ordering::SeqCst), 1);
    assert_eq!(turn.messages.len(), 3);
    assert_eq!(journal.finished.load(Ordering::SeqCst), 1);
    assert_eq!(script.requests.lock().unwrap().len(), 1);
    assert_eq!(journal.checkpoints.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn failed_terminal_journal_write_cannot_report_a_completed_run() {
    let script = Script::new(vec![Step::Reply(reply("candidate"))]);
    let executor = executor(script, RunBudget::default());
    let journal = FailingJournal {
        fail_tools: false,
        checkpoints: Mutex::new(vec![]),
        finished: AtomicUsize::new(0),
    };
    let events = Arc::new(Mutex::new(vec![]));
    let target = events.clone();
    let handler = Arc::new(move |event: &AgentRawEventEnvelope| {
        target.lock().unwrap().push(event.event.clone())
    });
    let turn = stopped(
        executor
            .run_with_journal(request(), handler, TurnExecutionContext::new(), &journal)
            .await,
    );
    assert!(matches!(&turn.stop,RunStop::Failed(f) if f.stage==RunStage::Persistence));
    assert_eq!(turn.messages[1].text_content(), "candidate");
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, AgentRawEvent::AgentCompleted { .. }))
    );
}
