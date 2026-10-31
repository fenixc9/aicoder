use aicoder_core::{TurnExecutionContext, ui::*};
use std::time::Duration;
fn input() -> UiPrompt {
    UiPrompt::Input {
        title: "Name".into(),
        initial: String::new(),
        multiline: false,
        max_bytes: 16,
    }
}
async fn question(host: &UiHost) -> UiQuestion {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = host.snapshot();
            if let Some(question) = snapshot.questions.first() {
                return question.clone();
            }
            host.wait(snapshot.revision).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn headless_and_unoffered_capabilities_fail_immediately() {
    let ui = UiClient::default();
    assert_eq!(ui.ask(input()).await, Err(UiError::Unsupported));
    assert_eq!(ui.status("work", None), Err(UiError::Unsupported));
    let host = UiHost::new(UiCapabilities::default(), UiLimits::default()).unwrap();
    assert_eq!(
        host.client("demo").unwrap().ask(input()).await,
        Err(UiError::Unsupported)
    );
}
#[tokio::test]
async fn typed_answers_reject_invalid_duplicate_and_foreign_replies() {
    let host = UiHost::interactive();
    let ui = host.client("demo").unwrap();
    let worker = tokio::spawn(async move {
        ui.ask(UiPrompt::Select {
            title: "Choose".into(),
            options: vec![UiOption {
                id: "a".into(),
                label: "A".into(),
            }],
        })
        .await
    });
    let q = question(&host).await;
    let foreign = UiHost::interactive();
    assert_eq!(
        foreign.answer(q.id, UiAnswer::Dismissed),
        Err(UiError::Stale)
    );
    assert_eq!(
        host.answer(q.id, UiAnswer::Text { text: "a".into() }),
        Err(UiError::Invalid)
    );
    assert_eq!(
        host.answer(
            q.id,
            UiAnswer::Choice {
                id: "missing".into()
            }
        ),
        Err(UiError::Invalid)
    );
    host.answer(q.id, UiAnswer::Choice { id: "a".into() })
        .unwrap();
    assert_eq!(
        worker.await.unwrap().unwrap(),
        UiAnswer::Choice { id: "a".into() }
    );
    assert_eq!(host.answer(q.id, UiAnswer::Dismissed), Err(UiError::Stale));
}
#[tokio::test]
async fn cancellation_and_dropped_futures_expire_questions_without_late_replies() {
    let host = UiHost::interactive();
    let context = TurnExecutionContext::new().with_ui(host.client("demo").unwrap());
    let ui = context.ui();
    let worker = tokio::spawn(async move { ui.ask(input()).await });
    let q = question(&host).await;
    context.cancel("stop");
    assert_eq!(host.answer(q.id, UiAnswer::Dismissed), Err(UiError::Stale));
    assert_eq!(worker.await.unwrap(), Err(UiError::Cancelled));
    let ui = host.client("other").unwrap();
    let worker = tokio::spawn(async move { ui.ask(input()).await });
    let q = question(&host).await;
    worker.abort();
    let _ = worker.await;
    assert!(host.snapshot().questions.is_empty());
    assert_eq!(host.answer(q.id, UiAnswer::Dismissed), Err(UiError::Stale));
}
#[tokio::test]
async fn dropping_last_host_owner_wakes_waiters_and_weak_clients_cannot_revive_it() {
    let host = UiHost::interactive();
    let ui = host.client("demo").unwrap();
    let retained = ui.clone();
    let worker = tokio::spawn(async move { ui.ask(input()).await });
    question(&host).await;
    drop(host);
    assert_eq!(worker.await.unwrap(), Err(UiError::Closed));
    assert_eq!(retained.ask(input()).await, Err(UiError::Closed));
}
#[tokio::test(start_paused = true)]
async fn deadlines_expire_and_invalid_answers_preserve_valid_questions() {
    let mut limits = UiLimits::default();
    limits.question_timeout = Duration::from_secs(1);
    let host = UiHost::new(UiCapabilities::interactive(), limits).unwrap();
    let ui = host.client("demo").unwrap();
    let worker = tokio::spawn(async move { ui.ask(input()).await });
    let q = question(&host).await;
    assert_eq!(
        host.answer(
            q.id,
            UiAnswer::Text {
                text: "x".repeat(17)
            }
        ),
        Err(UiError::Invalid)
    );
    assert_eq!(host.snapshot().questions.len(), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(host.snapshot().questions.is_empty());
    assert_eq!(worker.await.unwrap(), Err(UiError::TimedOut));
}
#[tokio::test]
async fn pending_status_notice_and_scope_operation_budgets_are_independent_and_bounded() {
    let mut limits = UiLimits::default();
    limits.max_questions = 1;
    limits.max_notices = 1;
    limits.max_statuses = 1;
    limits.max_operations = 8;
    let host = UiHost::new(UiCapabilities::interactive(), limits).unwrap();
    let ui = host.client("demo").unwrap();
    let caller = ui.clone();
    let worker = tokio::spawn(async move { caller.ask(input()).await });
    let q = question(&host).await;
    assert_eq!(ui.ask(input()).await, Err(UiError::Full));
    ui.status("work", Some("one".into())).unwrap();
    ui.status("work", Some("two".into())).unwrap();
    assert_eq!(ui.status("extra", Some("no".into())), Err(UiError::Full));
    ui.notify(UiLevel::Info, "first".into()).unwrap();
    ui.notify(UiLevel::Warning, "second".into()).unwrap();
    let snapshot = host.snapshot();
    assert_eq!(snapshot.omitted_notices, 1);
    assert_eq!(snapshot.notices[0].text, "second");
    assert_eq!(snapshot.statuses[0].text, "two");
    ui.status("work", None).unwrap();
    ui.notify(UiLevel::Info, "three".into()).unwrap();
    ui.notify(UiLevel::Info, "four".into()).unwrap();
    assert_eq!(ui.notify(UiLevel::Info, "five".into()), Err(UiError::Full));
    host.answer(q.id, UiAnswer::Dismissed).unwrap();
    assert_eq!(worker.await.unwrap().unwrap(), UiAnswer::Dismissed);
}
#[tokio::test]
async fn unsafe_text_and_invalid_prompt_shapes_are_not_published() {
    let host = UiHost::interactive();
    let ui = host.client("demo").unwrap();
    assert_eq!(
        ui.notify(UiLevel::Info, "\x1b[2J".into()),
        Err(UiError::Invalid)
    );
    assert_eq!(
        ui.ask(UiPrompt::Input {
            title: "Input".into(),
            initial: "bad\nline".into(),
            multiline: false,
            max_bytes: 16
        })
        .await,
        Err(UiError::Invalid)
    );
    assert!(host.snapshot().questions.is_empty());
    assert_eq!(host.snapshot().revision, 0);
}

mod execution {
    use super::*;
    use aicoder_core::{extensions::*, model::*, tools::*, *};
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    struct Provider;
    #[async_trait]
    impl ModelProvider for Provider {
        fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
            let mut spec = ModelSpec::new("offline", model);
            spec.capabilities.tools = true;
            Ok(spec)
        }
        async fn generate(
            &self,
            request: ModelRequest,
            sink: Arc<dyn ModelEventSink>,
            _: TurnExecutionContext,
        ) -> Result<ModelResponse, ProviderError> {
            let call = request
                .messages
                .last()
                .is_some_and(|m| m.role == MessageRole::User);
            let message = if call {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "one".into(),
                            name: "request_data".into(),
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
                stop_reason: if call {
                    StopReason::ToolCalls
                } else {
                    StopReason::Complete
                },
                usage: ModelUsage::zero(),
            };
            emit_response(&response, sink.as_ref());
            Ok(response)
        }
    }
    struct AskTool(Arc<Mutex<Option<UiClient>>>);
    #[async_trait]
    impl ExecutableTool for AskTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "request_data".into(),
                description: Some("Collect host data".into()),
                parameters: json!({"type":"object"}),
            }
        }
        fn capability(&self) -> ToolCapability {
            ToolCapability::ReadOnly
        }
        async fn execute(
            &self,
            context: &ToolContext,
            _: Value,
        ) -> Result<ToolSuccess, ToolFailure> {
            *self.0.lock().unwrap() = Some(context.ui().clone());
            let answer = match context.ui().ask(input()).await {
                Ok(answer) => answer,
                Err(UiError::Unsupported) => UiAnswer::Text {
                    text: "headless fallback".into(),
                },
                Err(error) => return Err(ToolFailure::new("ui", error.to_string())),
            };
            Ok(ToolSuccess::new(json!({"answer":answer})))
        }
    }
    #[tokio::test]
    async fn tool_ui_is_scoped_to_dispatch_and_only_explicit_tool_data_reaches_history() {
        let root = tempfile::tempdir().unwrap();
        let retained = Arc::new(Mutex::new(None));
        let mut tools = ToolRegistry::default();
        tools.register(AskTool(retained.clone())).unwrap();
        let agent = Agent::new(
            TurnExecutor::builder(Provider)
                .workspace(root.path())
                .registry(tools)
                .build()
                .unwrap(),
            AgentConfig::new("offline"),
        );
        let host = UiHost::interactive();
        let context = TurnExecutionContext::new().with_ui(host.client("extension").unwrap());
        let worker = tokio::spawn(async move {
            agent
                .run_with_context("collect", Arc::new(()), context)
                .await
        });
        let q = question(&host).await;
        assert_eq!(q.owner, "request_data");
        host.answer(
            q.id,
            UiAnswer::Text {
                text: "operator data".into(),
            },
        )
        .unwrap();
        let result = worker.await.unwrap().unwrap();
        let history = serde_json::to_string(&result.execution_result.progress.messages).unwrap();
        assert!(history.contains("operator data"));
        assert!(!history.contains(&q.id.scope.to_string()));
        assert!(!history.contains("\"title\":\"Name\""));
        assert_eq!(
            retained
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .notify(UiLevel::Info, "late".into()),
            Err(UiError::Cancelled)
        );
    }
    #[tokio::test]
    async fn a_headless_agent_uses_the_same_loop_with_explicit_extension_fallback() {
        let root = tempfile::tempdir().unwrap();
        let mut tools = ToolRegistry::default();
        tools.register(AskTool(Arc::new(Mutex::new(None)))).unwrap();
        let agent = Agent::new(
            TurnExecutor::builder(Provider)
                .workspace(root.path())
                .registry(tools)
                .build()
                .unwrap(),
            AgentConfig::new("offline"),
        );
        let result = agent.run("collect", Arc::new(())).await.unwrap();
        assert!(
            serde_json::to_string(&result.execution_result.progress.messages)
                .unwrap()
                .contains("headless fallback")
        );
    }
    struct Command(Arc<Mutex<Option<UiClient>>>);
    #[async_trait]
    impl ExtensionCommand for Command {
        fn specification(&self) -> CommandSpec {
            CommandSpec {
                name: "ask".into(),
                description: "Ask".into(),
                parameters: json!({"type":"object"}),
            }
        }
        async fn execute(
            &self,
            _: CommandRequest<'_>,
            cancellation: ExtensionCancellation,
        ) -> anyhow::Result<CommandOutput> {
            *self.0.lock().unwrap() = Some(cancellation.ui());
            cancellation.ui().ask(input()).await?;
            Ok(CommandOutput::Notice("answered".into()))
        }
    }
    #[tokio::test(start_paused = true)]
    async fn callback_timeout_expires_questions_and_retained_ui_capabilities() {
        let root = tempfile::tempdir().unwrap();
        let retained = Arc::new(Mutex::new(None));
        let mut ext = Extension::new("test", "1").unwrap();
        ext.command(Arc::new(Command(retained.clone())), Duration::from_secs(1))
            .unwrap();
        let mut registry = ExtensionRegistry::new();
        registry.install(ext).unwrap();
        let extensions = registry.finish();
        let host = UiHost::interactive();
        let context = TurnExecutionContext::new().with_ui(host.client("extension").unwrap());
        let worker = tokio::spawn(async move {
            extensions
                .execute_command(
                    "ask",
                    CommandRequest {
                        context: CommandContext {
                            workspace: root.path(),
                            session: None,
                        },
                        arguments: &json!({}),
                    },
                    &context,
                )
                .await
        });
        let q = question(&host).await;
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(
            worker
                .await
                .unwrap()
                .err()
                .unwrap()
                .to_string()
                .contains("timed out")
        );
        assert!(host.snapshot().questions.is_empty());
        assert_eq!(host.answer(q.id, UiAnswer::Dismissed), Err(UiError::Stale));
        assert_eq!(
            retained
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .notify(UiLevel::Info, "late".into()),
            Err(UiError::Cancelled)
        );
    }
    struct ChooseContext;
    #[async_trait]
    impl ContextHook for ChooseContext {
        async fn context(
            &self,
            _: ContextHookInput<'_>,
            cancellation: ExtensionCancellation,
        ) -> anyhow::Result<Option<String>> {
            let UiAnswer::Text { text } = cancellation.ui().ask(input()).await? else {
                return Ok(None);
            };
            Ok(Some(format!("Host context choice: {text}")))
        }
    }
    #[tokio::test]
    async fn context_ui_augments_only_the_runtime_prefix() {
        let root = tempfile::tempdir().unwrap();
        let mut extension = Extension::new("ctx", "1").unwrap();
        extension
            .context_hook(HookSpec::new("choose"), Arc::new(ChooseContext))
            .unwrap();
        let mut registry = ExtensionRegistry::new();
        registry.install(extension).unwrap();
        let executor = TurnExecutor::builder(Provider)
            .workspace(root.path())
            .extensions(Arc::new(registry.finish()))
            .build()
            .unwrap();
        let host = UiHost::interactive();
        let context = TurnExecutionContext::new().with_ui(host.client("extension").unwrap());
        let history = vec![Message::text(MessageRole::User, "canonical user")];
        let expected = history.clone();
        let worker = tokio::spawn(async move {
            executor
                .prepare_input(
                    ExecutionInput::from_request(ModelRequest::new("offline", history)),
                    &context,
                )
                .await
        });
        let q = question(&host).await;
        assert_eq!(q.owner, "ctx");
        host.answer(
            q.id,
            UiAnswer::Text {
                text: "bounded choice".into(),
            },
        )
        .unwrap();
        let input = worker.await.unwrap().unwrap();
        assert_eq!(
            input
                .history()
                .iter()
                .filter(|message| message.role == MessageRole::User)
                .cloned()
                .collect::<Vec<_>>(),
            expected
        );
        assert!(input.request().messages.iter().any(|message| {
            message.role == MessageRole::System
                && message
                    .text_content()
                    .contains("Host context choice: bounded choice")
        }));
        assert!(host.snapshot().questions.is_empty());
    }
}

#[tokio::test]
async fn expired_clients_cannot_be_revived_by_rebinding_to_fresh_execution_contexts() {
    let host = UiHost::interactive();
    let parent = TurnExecutionContext::new().with_ui(host.client("host").unwrap());
    let context = TurnExecutionContext::new().with_ui(parent.ui());
    let ui = context.ui();
    let worker = tokio::spawn(async move { ui.ask(input()).await });
    let q = question(&host).await;
    parent.cancel("parent scope ended");
    assert!(!context.is_cancelled());
    assert_eq!(worker.await.unwrap(), Err(UiError::Cancelled));
    assert_eq!(host.answer(q.id, UiAnswer::Dismissed), Err(UiError::Stale));
    let rebound = TurnExecutionContext::new().with_ui(parent.ui());
    assert_eq!(rebound.ui().ask(input()).await, Err(UiError::Cancelled));
    assert_eq!(
        rebound.ui().notify(UiLevel::Info, "late".into()),
        Err(UiError::Cancelled)
    );
    assert!(host.snapshot().questions.is_empty());
}
#[tokio::test]
async fn rebinding_depth_is_bounded_and_failure_does_not_publish_a_question() {
    let host = UiHost::interactive();
    let mut client = host.client("host").unwrap();
    for _ in 0..=MAX_UI_SCOPE_DEPTH {
        client = TurnExecutionContext::new().with_ui(client).ui();
    }
    assert_eq!(client.ask(input()).await, Err(UiError::Full));
    assert!(host.snapshot().questions.is_empty());
    assert_eq!(
        UiClient::default().ask(input()).await,
        Err(UiError::Unsupported)
    );
}

#[test]
fn interactive_command_deadlines_are_explicit_and_bounded_separately_from_hooks() {
    use aicoder_core::extensions::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    struct Command;
    #[async_trait]
    impl ExtensionCommand for Command {
        fn specification(&self) -> CommandSpec {
            CommandSpec {
                name: "dialog".into(),
                description: "Bounded dialog".into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        }
        async fn execute(
            &self,
            _: CommandRequest<'_>,
            _: ExtensionCancellation,
        ) -> anyhow::Result<CommandOutput> {
            Ok(CommandOutput::Notice("done".into()))
        }
    }
    let mut extension = Extension::new("interactive", "1").unwrap();
    extension
        .command(Arc::new(Command), Duration::from_secs(300))
        .unwrap();
    assert!(
        extension
            .command(Arc::new(Command), Duration::from_secs(601))
            .is_err()
    );
    assert!(
        extension
            .command(Arc::new(Command), Duration::ZERO)
            .is_err()
    );
}
