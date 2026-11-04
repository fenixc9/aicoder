use aicoder_core::{context::*, extensions::*, model::*, session::*, summary::*, tools::*, *};
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
    checkpoint: Option<(Arc<MemorySessionRepository>, String)>,
    checkpoint_at_main: Mutex<Vec<bool>>,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", model);
        s.context_tokens = Some(if model == "small" { 256 } else { 16384 });
        s.max_output_tokens = Some(8);
        s.capabilities.tools = true;
        s.capabilities.temperature = true;
        s.capabilities.reasoning_efforts = vec![ReasoningEffort::Low];
        Ok(s)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        request.validate(&self.model_spec(&request.model)?)?;
        let summary = request
            .messages
            .first()
            .is_some_and(|m| m.text_content().starts_with("Summarize historical"));
        if !summary && let Some((repo, id)) = &self.checkpoint {
            self.checkpoint_at_main.lock().unwrap().push(
                repo.open(id)
                    .unwrap()
                    .selected_branch()
                    .model_context()
                    .unwrap()
                    .source
                    .summary_checkpoint
                    .is_some(),
            );
        }
        self.requests.lock().unwrap().push(request.clone());
        let tools =
            !summary && self.tools && request.messages.last().unwrap().role == MessageRole::User;
        let message = if summary {
            Message::text(MessageRole::Assistant,json!({"goals":["retain source facts"],"constraints":[],"changes":[],"todos":[],"notes":[]}).to_string())
        } else if tools {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "c".into(),
                        name: "echo".into(),
                        arguments: json!({}),
                    },
                }],
            }
        } else {
            Message::text(MessageRole::Assistant, "done")
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
fn set(hook: Arc<dyn RequestHook>, scope: RequestHookScope) -> Arc<ExtensionSet> {
    let mut e = Extension::new("view", "1").unwrap();
    e.request_hook(HookSpec::new("edit"), scope, hook).unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    Arc::new(r.finish())
}
struct Hook {
    mode: &'static str,
    calls: Arc<Mutex<Vec<RequestHookPass>>>,
}
#[async_trait]
impl RequestHook for Hook {
    async fn request(
        &self,
        input: RequestHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<RequestEdit>> {
        self.calls.lock().unwrap().push(input.pass);
        let mut edit = RequestEdit::default();
        match self.mode {
            "filter" => {
                let mut messages = input.request.messages[..input.system_prefix_len].to_vec();
                if let Some(last) = input.request.messages.last()
                    && last.role != MessageRole::System
                {
                    messages.push(Message::text(last.role, "filtered"));
                }
                edit.messages = Some(RequestMessages {
                    messages,
                    system_prefix_len: input.system_prefix_len,
                });
            }
            "system" => {
                let mut messages = input.request.messages.clone();
                messages[0] = Message::text(MessageRole::System, "HOOK_ONLY_SYSTEM");
                edit.messages = Some(RequestMessages {
                    messages,
                    system_prefix_len: input.system_prefix_len,
                });
            }
            "append" => {
                let mut messages = input.request.messages.clone();
                messages.push(Message::text(MessageRole::System, "HOOK_ONLY_CONTEXT"));
                edit.messages = Some(RequestMessages {
                    messages,
                    system_prefix_len: input.system_prefix_len,
                });
            }
            "inflate" => {
                edit.messages = Some(RequestMessages {
                    messages: input.request.messages[..input.system_prefix_len]
                        .iter()
                        .cloned()
                        .chain([Message::text(MessageRole::User, "x".repeat(10000))])
                        .collect(),
                    system_prefix_len: input.system_prefix_len,
                })
            }
            "hide" => {
                edit.tools = Some(vec![]);
                edit.tool_choice = Some(ModelToolChoice::Auto);
            }
            "unknown_tool" => edit.tools = Some(vec!["missing".into()]),
            "invalid_unit" => {
                edit.messages = Some(RequestMessages {
                    messages: vec![Message {
                        role: MessageRole::Tool,
                        content: vec![ContentBlock::ToolResult {
                            call_id: "invented".into(),
                            is_error: false,
                            content: json!({}),
                        }],
                    }],
                    system_prefix_len: 0,
                })
            }
            "options" => {
                assert!(input.request.options.temperature.is_none());
                assert!(input.request.options.reasoning_effort.is_none());
                let mut options = input.request.options.clone();
                options.temperature = Some(0.25);
                options.reasoning_effort = Some(ReasoningEffort::Low);
                edit.options = Some(options);
            }
            "invalid_options" => {
                let mut options = input.request.options.clone();
                options.top_p = Some(0.5);
                edit.options = Some(options);
            }
            "native_error" => anyhow::bail!("private-native-error"),
            "noop" => return Ok(None),
            _ => unreachable!(),
        }
        Ok(Some(edit))
    }
}
fn make_hook(mode: &'static str) -> (Arc<Hook>, Arc<Mutex<Vec<RequestHookPass>>>) {
    let calls = Arc::new(Mutex::new(vec![]));
    (
        Arc::new(Hook {
            mode,
            calls: calls.clone(),
        }),
        calls,
    )
}
fn model() -> Arc<Provider> {
    Arc::new(Provider::default())
}
fn input(model: &str, long: bool) -> ModelRequest {
    ModelRequest::new(
        model,
        vec![
            Message::text(MessageRole::System, "host prefix"),
            Message::text(
                MessageRole::User,
                if long {
                    "x".repeat(12000)
                } else {
                    "user".into()
                },
            ),
        ],
    )
}
#[tokio::test]
async fn request_filtering_precedes_window_admission_and_never_rewrites_source_history() {
    let root = tempfile::tempdir().unwrap();
    let provider = model();
    let (hook, calls) = make_hook("filter");
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(set(hook, RequestHookScope::Conversation))
        .build()
        .unwrap();
    let original = input("small", true);
    let outcome = executor
        .run_input_outcome(
            ExecutionInput::from_request(original.clone()),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(&outcome.messages[..2], original.messages.as_slice());
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].messages[1].text_content(), "filtered");
    assert_eq!(*calls.lock().unwrap(), vec![RequestHookPass::Admission]);
    assert!(
        outcome
            .model_context
            .as_ref()
            .unwrap()
            .last_input_count
            .as_ref()
            .unwrap()
            .tokens
            < 256
    );
}
#[tokio::test]
async fn explicit_system_scope_changes_only_the_ephemeral_prefix() {
    for scope in [
        RequestHookScope::Conversation,
        RequestHookScope::SystemAndConversation,
    ] {
        let root = tempfile::tempdir().unwrap();
        let provider = model();
        let (hook, _) = make_hook("system");
        let executor = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(hook, scope))
            .build()
            .unwrap();
        let original = input("m", false);
        let outcome = executor
            .run_input_outcome(
                ExecutionInput::from_request(original.clone()),
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        if scope == RequestHookScope::Conversation {
            assert!(matches!(outcome, RunOutcome::Interrupted(_)));
            assert!(provider.requests.lock().unwrap().is_empty());
        } else {
            let result = outcome.into_result().unwrap();
            assert_eq!(result.messages[0], original.messages[0]);
            assert_eq!(
                provider.requests.lock().unwrap()[0].messages[0].text_content(),
                "HOOK_ONLY_SYSTEM"
            );
        }
    }
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
        Ok(ToolSuccess::new(json!("echo")))
    }
}
#[tokio::test]
async fn request_tool_subsets_are_enforced_by_actual_dispatch_and_never_refill_empty_catalogs() {
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(Provider {
        tools: true,
        ..Default::default()
    });
    let executed = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry.register(Echo(executed.clone())).unwrap();
    let (hook, _) = make_hook("hide");
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .registry(registry)
            .extensions(set(hook, RequestHookScope::Conversation))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let turn = agent.run("run", Arc::new(())).await.unwrap();
    assert_eq!(executed.load(Ordering::SeqCst), 0);
    let result = turn
        .execution_result
        .messages
        .iter()
        .find_map(Message::tool_result)
        .unwrap();
    assert_eq!(result.1["error"]["code"], "tool_not_exposed");
    assert!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.tools.is_empty())
    );
}
#[tokio::test]
async fn options_are_request_local_and_preparation_is_frozen_once_per_first_turn() {
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(Provider {
        tools: true,
        ..Default::default()
    });
    let executed = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry.register(Echo(executed)).unwrap();
    let (hook, calls) = make_hook("options");
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .registry(registry)
            .extensions(set(hook, RequestHookScope::Conversation))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "run",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        vec![RequestHookPass::Preparation, RequestHookPass::Admission]
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| r.options.temperature == Some(0.25)
        && r.options.reasoning_effort == Some(ReasoningEffort::Low)));
    let saved = repo.open(&session.metadata().id).unwrap();
    assert!(
        saved
            .reasoning_selection()
            .unwrap()
            .is_none_or(|v| v.effort.is_none())
    );
    let ready = saved
        .entries()
        .iter()
        .filter_map(|e| match &e.data {
            SessionEntryData::Extension { namespace, data }
                if namespace == REQUEST_VIEW_NAMESPACE && data["stage"] == "ready" =>
            {
                Some(data)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ready.len(), 2);
    assert!(
        ready
            .iter()
            .all(|r| r["effective_options"]["temperature"] == 0.25
                && r["effective_options"]["reasoning_effort"] == "low")
    );
}
#[tokio::test]
async fn invalid_edits_and_inflation_cannot_bypass_capabilities_windows_or_parent_tokens() {
    for mode in [
        "unknown_tool",
        "invalid_unit",
        "invalid_options",
        "inflate",
        "native_error",
    ] {
        let root = tempfile::tempdir().unwrap();
        let provider = model();
        let (hook, _) = make_hook(mode);
        let executor = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(hook, RequestHookScope::SystemAndConversation))
            .build()
            .unwrap();
        let outcome = executor
            .run_input_outcome(
                ExecutionInput::from_request(input("small", false)),
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(matches!(outcome, RunOutcome::Interrupted(_)), "{mode}");
        assert!(provider.requests.lock().unwrap().is_empty());
        if mode == "inflate" {
            assert!(
                outcome
                    .stop()
                    .unwrap()
                    .to_string()
                    .contains("model_context_exceeded")
            );
        }
        assert!(
            !outcome
                .stop()
                .unwrap()
                .to_string()
                .contains("private-native-error")
        );
    }
    let root = tempfile::tempdir().unwrap();
    let provider = model();
    let (hook, _) = make_hook("append");
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(set(hook, RequestHookScope::Conversation))
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_tokens: Some(1),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let outcome = executor
        .run_input_outcome(
            ExecutionInput::from_request(input("m", false)),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(outcome,RunOutcome::Interrupted(ref i)if matches!(i.stop,RunStop::Budget(_))));
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
fn summary_config() -> SummaryConfig {
    let mut c = SummaryConfig::new(ContextWindowConfig {
        max_context_tokens: 1200,
        reserved_output_tokens: 200,
        preserve_recent_tokens: 80,
    });
    c.output_tokens = 128;
    c.max_input_tokens = 1400;
    c
}
fn old_history() -> Vec<Message> {
    (0..8)
        .map(|i| {
            Message::text(
                if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                format!("old-{i} {}", "x".repeat(1000)),
            )
        })
        .collect()
}
#[tokio::test]
async fn unsupported_projection_journals_fail_before_summary_or_main_provider_io() {
    for mode in ["append", "noop"] {
        let root = tempfile::tempdir().unwrap();
        let provider = model();
        let (hook, _) = make_hook(mode);
        let executor = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(hook, RequestHookScope::Conversation))
            .context_compactor(
                SemanticContextCompactor::new(provider.clone(), "m", summary_config()).unwrap(),
            )
            .build()
            .unwrap();
        let mut request = input("m", false);
        request.messages.splice(1..1, old_history());
        let outcome = executor
            .run_with_journal(
                request,
                Arc::new(()),
                TurnExecutionContext::new(),
                &Unsupported,
            )
            .await;
        assert!(
            matches!(outcome,RunOutcome::Interrupted(ref i)if matches!(&i.stop,RunStop::Failed(f)if f.stage==RunStage::Persistence))
        );
        assert!(provider.requests.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn compaction_summarizes_raw_facts_commits_checkpoint_then_rebuilds_the_request_view() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let mut session = repo.create(root.path()).unwrap();
    for message in old_history() {
        repo.append(&mut session, message).unwrap();
    }
    let provider = Arc::new(Provider {
        checkpoint: Some((repo.clone(), session.metadata().id.clone())),
        ..Default::default()
    });
    let (hook, calls) = make_hook("append");
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(hook, RequestHookScope::Conversation))
            .context_compactor(
                SemanticContextCompactor::new(provider.clone(), "m", summary_config()).unwrap(),
            )
            .build()
            .unwrap(),
        AgentConfig::new("m").system_prompt("host prefix"),
    );
    let turn = agent
        .run_with_session_outcome(
            repo.as_ref(),
            SessionSelection::Existing(session.metadata().id.clone()),
            "latest",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    let requests = provider.requests.lock().unwrap();
    let summaries = requests
        .iter()
        .filter(|r| {
            r.messages[0]
                .text_content()
                .starts_with("Summarize historical")
        })
        .collect::<Vec<_>>();
    assert!(!summaries.is_empty());
    assert!(summaries.iter().all(|r| {
        !serde_json::to_string(r)
            .unwrap()
            .contains("HOOK_ONLY_CONTEXT")
    }));
    assert_eq!(*provider.checkpoint_at_main.lock().unwrap(), vec![true]);
    assert!(
        calls
            .lock()
            .unwrap()
            .contains(&RequestHookPass::AfterCompaction)
    );
    let saved = repo.open(&session.metadata().id).unwrap();
    assert!(
        !serde_json::to_string(saved.entries())
            .unwrap()
            .contains("HOOK_ONLY_CONTEXT")
    );
    assert!(
        turn.execution_result
            .model_context
            .as_ref()
            .unwrap()
            .compactions
            > 0
    );
    assert!(
        saved
            .messages()
            .iter()
            .any(|m| m.message.text_content().starts_with("old-0"))
    );
}
#[tokio::test]
async fn selection_checks_the_frozen_projection_and_restored_metadata_never_reinstalls_hooks() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append(
        &mut session,
        Message::text(MessageRole::User, "x".repeat(12000)),
    )
    .unwrap();
    repo.append(
        &mut session,
        Message::text(MessageRole::Assistant, "old reply"),
    )
    .unwrap();
    let provider = model();
    let (hook, calls) = make_hook("filter");
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(hook, RequestHookScope::Conversation))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    )
    .following_session_models();
    let report = agent
        .switch_session_model(
            &repo,
            &session.metadata().id,
            "small",
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert!(report.input_token_estimate < 256);
    assert!(provider.requests.lock().unwrap().is_empty());
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "latest",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()
        .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    let before = repo.open(&session.metadata().id).unwrap().entries().len();
    let fresh = Agent::new(
        TurnExecutor::builder_from_shared(model())
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("small"),
    );
    let outcome = fresh
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "without hook",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
    assert_eq!(
        repo.open(&session.metadata().id).unwrap().entries().len(),
        before
    );
}
struct Slow {
    started: Arc<tokio::sync::Notify>,
    cancelled: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl RequestHook for Slow {
    async fn request(
        &self,
        _: RequestHookInput<'_>,
        c: ExtensionCancellation,
    ) -> Result<Option<RequestEdit>> {
        let ended = self.cancelled.clone();
        tokio::spawn(async move {
            c.cancelled().await;
            ended.notify_one();
        });
        self.started.notify_one();
        std::future::pending().await
    }
}
#[tokio::test]
async fn cancellation_and_deadlines_expire_callback_children_without_requests_or_session_writes() {
    for cancel in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let repo = Arc::new(MemorySessionRepository::new());
        let session = repo.create(root.path()).unwrap();
        let count = session.entries().len();
        let provider = model();
        let started = Arc::new(tokio::sync::Notify::new());
        let cancelled = Arc::new(tokio::sync::Notify::new());
        let mut e = Extension::new("slow", "1").unwrap();
        e.request_hook(
            HookSpec {
                timeout: Duration::from_millis(30),
                ..HookSpec::new("slow")
            },
            RequestHookScope::Conversation,
            Arc::new(Slow {
                started: started.clone(),
                cancelled: cancelled.clone(),
            }),
        )
        .unwrap();
        let mut r = ExtensionRegistry::new();
        r.install(e).unwrap();
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(provider.clone())
                .workspace(root.path())
                .extensions(Arc::new(r.finish()))
                .build()
                .unwrap(),
            AgentConfig::new("m"),
        );
        let context = TurnExecutionContext::new();
        let worker = context.clone();
        let session_id = session.metadata().id.clone();
        let repo2 = repo.clone();
        let task = tokio::spawn(async move {
            agent
                .run_with_session_outcome(
                    repo2.as_ref(),
                    SessionSelection::Existing(session_id),
                    "latest",
                    Arc::new(()),
                    worker,
                )
                .await
        });
        started.notified().await;
        if cancel {
            context.cancel("cancel request projection");
        }
        let outcome = task.await.unwrap();
        assert!(!matches!(outcome, AgentTurnOutcome::Completed(_)));
        tokio::time::timeout(Duration::from_secs(1), cancelled.notified())
            .await
            .unwrap();
        assert!(provider.requests.lock().unwrap().is_empty());
        assert_eq!(
            repo.open(&session.metadata().id).unwrap().entries().len(),
            count
        );
    }
}

#[tokio::test]
async fn queued_input_invalidates_first_view_and_cross_executor_snapshots_are_not_reused() {
    let root = tempfile::tempdir().unwrap();
    let provider = model();
    let (hook, calls) = make_hook("append");
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(set(hook, RequestHookScope::Conversation))
        .build()
        .unwrap();
    let prepared = executor
        .prepare_input(
            ExecutionInput::from_request(input("m", false)),
            &TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let (handle, control) = RunHandle::pair();
    handle
        .steer(Message::text(MessageRole::User, "fresh control"))
        .unwrap();
    executor
        .run_input_controlled(prepared, Arc::new(()), control)
        .await
        .into_result()
        .unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        vec![RequestHookPass::Preparation, RequestHookPass::Admission]
    );
    assert!(
        provider.requests.lock().unwrap()[0]
            .messages
            .iter()
            .any(|m| m.text_content() == "fresh control")
    );
    let (hook, calls) = make_hook("system");
    let old = TurnExecutor::builder_from_shared(model())
        .workspace(root.path())
        .extensions(set(hook, RequestHookScope::SystemAndConversation))
        .build()
        .unwrap();
    let prepared = old
        .prepare_input(
            ExecutionInput::from_request(input("m", false)),
            &TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let target = model();
    let fresh = TurnExecutor::builder_from_shared(target.clone())
        .workspace(root.path())
        .build()
        .unwrap();
    fresh.run_input(prepared).await.unwrap();
    assert_eq!(
        target.requests.lock().unwrap()[0].messages[0].text_content(),
        "host prefix"
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
}
struct Ordered(&'static str, Arc<Mutex<Vec<&'static str>>>);
#[async_trait]
impl RequestHook for Ordered {
    async fn request(
        &self,
        input: RequestHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<RequestEdit>> {
        self.1.lock().unwrap().push(self.0);
        let mut messages = input.request.messages.clone();
        messages.push(Message::text(MessageRole::User, self.0));
        Ok(Some(RequestEdit {
            messages: Some(RequestMessages {
                messages,
                system_prefix_len: input.system_prefix_len,
            }),
            ..Default::default()
        }))
    }
}
#[tokio::test]
async fn request_hooks_chain_in_priority_and_stable_registration_order() {
    let root = tempfile::tempdir().unwrap();
    let provider = model();
    let order = Arc::new(Mutex::new(vec![]));
    let mut e = Extension::new("ordered", "1").unwrap();
    for (name, priority) in [("second", 10), ("first", -1), ("third", 10)] {
        e.request_hook(
            HookSpec {
                priority,
                ..HookSpec::new(name)
            },
            RequestHookScope::Conversation,
            Arc::new(Ordered(name, order.clone())),
        )
        .unwrap();
    }
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(Arc::new(r.finish()))
        .build()
        .unwrap();
    executor
        .run_input(ExecutionInput::from_request(input("m", false)))
        .await
        .unwrap();
    assert_eq!(*order.lock().unwrap(), vec!["first", "second", "third"]);
    let requests = provider.requests.lock().unwrap();
    assert_eq!(
        requests[0]
            .messages
            .iter()
            .skip(2)
            .map(Message::text_content)
            .collect::<Vec<_>>(),
        vec!["first", "second", "third"]
    );
}
#[test]
fn option_observations_mark_private_strings_and_schemas_incomplete_without_serializing_them() {
    let options = ModelOptions {
        stop: Some(vec!["PRIVATE_STOP".into()]),
        response_format: Some(ResponseFormat::JsonSchema {
            name: "private".into(),
            schema: json!({"description":"PRIVATE_SCHEMA"}),
            strict: true,
        }),
        ..Default::default()
    };
    let observed = RequestOptionSummary::from(&options);
    assert!(!observed.complete);
    let encoded = serde_json::to_string(&observed).unwrap();
    assert!(!encoded.contains("PRIVATE_STOP") && !encoded.contains("PRIVATE_SCHEMA"));
    assert_eq!(observed.stop_sequences, Some(1));
}

struct BlockingView;
#[async_trait]
impl RequestHook for BlockingView {
    async fn request(
        &self,
        input: RequestHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<RequestEdit>> {
        std::thread::sleep(Duration::from_millis(60));
        let mut messages = input.request.messages.clone();
        messages.push(Message::text(MessageRole::System, "blocking view"));
        Ok(Some(RequestEdit {
            messages: Some(RequestMessages {
                messages,
                system_prefix_len: input.system_prefix_len,
            }),
            ..Default::default()
        }))
    }
}
struct BlockingReady;
#[async_trait]
impl RunJournal for BlockingReady {
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn record_request_view(&self, record: &RequestViewRecord) -> Result<()> {
        if matches!(record.stage, RequestViewStage::Ready) {
            std::thread::sleep(Duration::from_millis(60));
        }
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn elapsed_native_or_journal_work_is_rechecked_before_summary_and_main_provider_io() {
    for summary in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let provider = model();
        let hook: Arc<dyn RequestHook> = if summary {
            Arc::new(BlockingView)
        } else {
            make_hook("append").0
        };
        let mut builder = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(hook, RequestHookScope::Conversation))
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_duration: Some(Duration::from_millis(30)),
                    ..Default::default()
                },
                ..Default::default()
            });
        let mut request = input("m", false);
        if summary {
            builder = builder.context_compactor(
                SemanticContextCompactor::new(provider.clone(), "m", summary_config()).unwrap(),
            );
            request.messages.splice(1..1, old_history());
        }
        let outcome = builder
            .build()
            .unwrap()
            .run_with_journal(
                request,
                Arc::new(()),
                TurnExecutionContext::new(),
                &BlockingReady,
            )
            .await;
        assert!(
            matches!(outcome,RunOutcome::Interrupted(ref i)if matches!(i.stop,RunStop::Budget(_)))
        );
        assert!(
            provider.requests.lock().unwrap().is_empty(),
            "summary={summary}: elapsed budget allowed provider IO"
        );
    }
}
