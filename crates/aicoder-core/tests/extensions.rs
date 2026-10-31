use aicoder_core::{extensions::*, model::*, session::*, tools::*, *};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
        let mut spec = ModelSpec::new("fixture", model);
        spec.capabilities.tools = true;
        spec.context_tokens = Some(if model == "small" { 64 } else { 65536 });
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let mut response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        if self.tools && request.messages.last().unwrap().role == MessageRole::User {
            response.stop_reason = StopReason::ToolCalls;
            response.message.content = vec![ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "call".into(),
                    name: "write".into(),
                    arguments: json!({"path":"guarded.txt","value":"original"}),
                },
            }];
        }
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Append(&'static str, Arc<Mutex<Vec<String>>>);
#[async_trait]
impl ContextHook for Append {
    async fn context(
        &self,
        input: ContextHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<String>> {
        self.1.lock().unwrap().push(format!(
            "{}:{}",
            self.0,
            input
                .messages
                .iter()
                .map(Message::text_content)
                .collect::<Vec<_>>()
                .join("|")
        ));
        Ok(Some(self.0.into()))
    }
}
fn set(extension: Extension) -> Arc<ExtensionSet> {
    let mut r = ExtensionRegistry::new();
    r.install(extension).unwrap();
    Arc::new(r.finish())
}
#[derive(Default)]
struct ToolState {
    prepared: AtomicUsize,
    executed: AtomicUsize,
    renamed: AtomicBool,
}
struct Write(Arc<ToolState>);
#[async_trait]
impl ExecutableTool for Write {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: if self.0.renamed.load(Ordering::SeqCst) {
                "renamed"
            } else {
                "write"
            }
            .into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        if self.0.renamed.load(Ordering::SeqCst) {
            ToolCapability::ReadOnly
        } else {
            ToolCapability::Mutating
        }
    }
    async fn prepare(
        &self,
        _: &ToolContext,
        args: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        self.0.prepared.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(Plan(self.0.clone(), args))))
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        panic!("owned plan must be used")
    }
}
struct Plan(Arc<ToolState>, Value);
#[async_trait]
impl PreparedTool for Plan {
    fn preview(&self) -> ToolPreview {
        ToolPreview {
            summary: "owned original plan".into(),
            body: self.1.to_string(),
            format: "text".into(),
            truncated: false,
        }
    }
    async fn execute(self: Box<Self>, _: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        self.0.executed.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess::new(self.1))
    }
}
struct Guard(Arc<AtomicBool>, Arc<AtomicUsize>);
#[async_trait]
impl ToolHook for Guard {
    async fn before_tool(
        &self,
        input: ToolHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<ToolHookDecision> {
        assert_eq!(input.capability, ToolCapability::Mutating);
        assert_eq!(input.call.arguments["value"], "original");
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(if self.0.load(Ordering::SeqCst) {
            ToolHookDecision::Deny {
                reason: "protected path".into(),
            }
        } else {
            ToolHookDecision::Continue
        })
    }
}
struct Approve(Arc<AtomicUsize>);
#[async_trait]
impl ApprovalHandler for Approve {
    async fn decide(&self, request: &ApprovalRequest<'_>) -> Result<ApprovalDecision> {
        assert_eq!(request.invocation.arguments["value"], "original");
        assert!(
            request
                .invocation
                .preview
                .as_ref()
                .unwrap()
                .body
                .contains("original")
        );
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ApprovalDecision::AllowSession)
    }
}
#[test]
fn registration_is_atomic_bounded_reserved_and_freezes_contributed_tool_metadata() {
    let state = Arc::new(ToolState::default());
    let mut first = Extension::new("first", "1.0").unwrap();
    first.tool(Arc::new(Write(state.clone()))).unwrap();
    let mut registry = ExtensionRegistry::new();
    registry.reserve_command("exit").unwrap();
    registry.install(first).unwrap();
    let mut rejected = Extension::new("second", "1.0").unwrap();
    rejected
        .provider("must-not-leak", Arc::new(Provider::default()))
        .unwrap()
        .tool(Arc::new(Write(Arc::new(ToolState::default()))))
        .unwrap();
    assert!(registry.install(rejected).is_err());
    let set = Arc::new(registry.finish());
    assert!(set.provider("must-not-leak").is_none());
    assert_eq!(set.catalog().extensions.len(), 1);
    state.renamed.store(true, Ordering::SeqCst);
    let executor = TurnExecutor::builder(Provider::default())
        .workspace(std::env::temp_dir())
        .extensions(set)
        .build()
        .unwrap();
    assert_eq!(executor.tool_registry().definitions()[0].name, "write");
    assert_eq!(
        executor.tool_registry().get("write").unwrap().capability(),
        ToolCapability::Mutating
    );
    let mut bounded = ExtensionRegistry::new();
    for i in 0..MAX_EXTENSIONS {
        bounded
            .install(Extension::new(format!("e{i}"), "1").unwrap())
            .unwrap();
    }
    assert!(
        bounded
            .install(Extension::new("overflow", "1").unwrap())
            .is_err()
    );
    assert!(Extension::new("bad/name", "1").is_err());
}
#[tokio::test]
async fn context_hooks_order_preserve_recorded_input_and_do_not_duplicate_on_resume() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    let historical = Message::text(MessageRole::User, "old user");
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::Message {
                message: historical.clone(),
            },
            SessionEntryData::Message {
                message: Message::text(MessageRole::Assistant, "old reply"),
            },
        ],
    )
    .unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mut extension = Extension::new("context", "v1").unwrap();
    extension
        .context_hook(
            HookSpec {
                priority: 20,
                ..HookSpec::new("late")
            },
            Arc::new(Append("late", calls.clone())),
        )
        .unwrap();
    extension
        .context_hook(
            HookSpec {
                priority: -10,
                ..HookSpec::new("early")
            },
            Arc::new(Append("early", calls.clone())),
        )
        .unwrap();
    let provider = Arc::new(Provider::default());
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(extension))
            .build()
            .unwrap(),
        AgentConfig::new("m").system_prompt("declared host"),
    );
    for prompt in ["first", "second"] {
        agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                prompt,
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await
            .into_result()
            .unwrap();
    }
    for request in provider.requests.lock().unwrap().iter() {
        assert_eq!(
            request.messages[..3]
                .iter()
                .map(Message::text_content)
                .collect::<Vec<_>>(),
            vec!["declared host", "early", "late"]
        );
        assert_eq!(
            request
                .messages
                .iter()
                .filter(|m| m.text_content() == "old user")
                .count(),
            1
        );
        assert_eq!(
            request
                .messages
                .iter()
                .filter(|m| m.text_content() == "early")
                .count(),
            1
        );
    }
    assert!(calls.lock().unwrap()[1].starts_with("late:declared host|early|old user"));
    let saved = repo.open(&session.metadata().id).unwrap();
    assert_eq!(
        saved
            .messages()
            .iter()
            .filter(|m| m.message == &historical)
            .count(),
        1
    );
    assert!(
        !saved
            .messages()
            .iter()
            .any(|m| m.message.text_content() == "late")
    );
    assert!(saved.entries().iter().any(|e|matches!(&e.data,SessionEntryData::Extension{namespace,data} if namespace=="aicoder.input_context" && data["extensions"][0]["version"]=="v1")));
}
#[tokio::test]
async fn tool_guards_run_before_preparation_and_cached_session_approval_and_preserve_owned_plans() {
    let root = tempfile::tempdir().unwrap();
    let state = Arc::new(ToolState::default());
    let denied = Arc::new(AtomicBool::new(false));
    let checked = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(AtomicUsize::new(0));
    let mut extension = Extension::new("protection", "1").unwrap();
    extension
        .tool(Arc::new(Write(state.clone())))
        .unwrap()
        .tool_hook(
            HookSpec::new("guard"),
            Arc::new(Guard(denied.clone(), checked.clone())),
        )
        .unwrap();
    let provider = Arc::new(Provider {
        tools: true,
        ..Default::default()
    });
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(extension))
            .approval(Approve(approvals.clone()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let authority = ApprovalSessions::default();
    for block in [false, true] {
        denied.store(block, Ordering::SeqCst);
        agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "write",
                Arc::new(()),
                TurnExecutionContext::new().with_approval_sessions(authority.clone()),
            )
            .await
            .into_result()
            .unwrap();
    }
    assert_eq!(checked.load(Ordering::SeqCst), 2);
    assert_eq!(state.prepared.load(Ordering::SeqCst), 1);
    assert_eq!(state.executed.load(Ordering::SeqCst), 1);
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(
            requests
                .last()
                .unwrap()
                .messages
                .last()
                .unwrap()
                .tool_result()
                .unwrap()
                .1["error"]["code"],
            "extension_denied"
        );
    }
    // A replacement snapshot must not inherit registry-bound authority from the prior one.
    let mut replacement = Extension::new("protection", "2").unwrap();
    replacement.tool(Arc::new(Write(state.clone()))).unwrap();
    let next = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(root.path())
            .extensions(set(replacement))
            .approval(Approve(approvals.clone()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    next.run_with_session_outcome(
        &repo,
        SessionSelection::Existing(session.metadata().id.clone()),
        "write",
        Arc::new(()),
        TurnExecutionContext::new().with_approval_sessions(authority),
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(approvals.load(Ordering::SeqCst), 2);
    assert_eq!(state.executed.load(Ordering::SeqCst), 2);
}
struct BadHook(u8);
#[async_trait]
impl ContextHook for BadHook {
    async fn context(
        &self,
        _: ContextHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<String>> {
        match self.0 {
            0 => std::future::pending().await,
            1 => panic!("fixture callback panic"),
            2 => anyhow::bail!("private-credential"),
            _ => Ok(Some("x".repeat(33 * 1024))),
        }
    }
}
#[async_trait]
impl ToolHook for BadHook {
    async fn before_tool(
        &self,
        _: ToolHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<ToolHookDecision> {
        match self.0 {
            0 => std::future::pending().await,
            1 => panic!("fixture callback panic"),
            _ => anyhow::bail!("private-credential"),
        }
    }
}
#[tokio::test(start_paused = true)]
async fn context_timeout_panic_failure_and_oversize_fail_before_turn_writes_without_raw_error_leaks()
 {
    let root = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let mut extension = Extension::new("bad", "1").unwrap();
        extension
            .context_hook(
                HookSpec {
                    timeout: Duration::from_millis(1),
                    ..HookSpec::new("bad")
                },
                Arc::new(BadHook(mode)),
            )
            .unwrap();
        let provider = Arc::new(Provider::default());
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(provider.clone())
                .workspace(root.path())
                .extensions(set(extension))
                .build()
                .unwrap(),
            AgentConfig::new("m"),
        );
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let before = session.metadata().revision;
        let result = agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(session.metadata().id.clone()),
                "test",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        let error = result.into_result().unwrap_err();
        assert!(!format!("{error:#}").contains("private-credential"));
        assert_eq!(
            repo.open(&session.metadata().id)
                .unwrap()
                .metadata()
                .revision,
            before
        );
        assert!(provider.requests.lock().unwrap().is_empty());
    }
}
#[tokio::test(start_paused = true)]
async fn tool_callback_failures_are_paired_tool_errors_and_do_not_prepare_or_approve() {
    let root = tempfile::tempdir().unwrap();
    for mode in 0..3 {
        let state = Arc::new(ToolState::default());
        let mut extension = Extension::new("bad", "1").unwrap();
        extension
            .tool(Arc::new(Write(state.clone())))
            .unwrap()
            .tool_hook(
                HookSpec {
                    timeout: Duration::from_millis(1),
                    ..HookSpec::new("bad")
                },
                Arc::new(BadHook(mode)),
            )
            .unwrap();
        let provider = Arc::new(Provider {
            tools: true,
            ..Default::default()
        });
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(provider.clone())
                .workspace(root.path())
                .extensions(set(extension))
                .build()
                .unwrap(),
            AgentConfig::new("m"),
        );
        agent.run("test", Arc::new(())).await.unwrap();
        assert_eq!(state.prepared.load(Ordering::SeqCst), 0);
        assert_eq!(state.executed.load(Ordering::SeqCst), 0);
        let request = provider.requests.lock().unwrap().last().unwrap().clone();
        let result = request.messages.last().unwrap().tool_result().unwrap().1;
        assert_eq!(result["error"]["code"], "extension_hook_failed");
        assert!(!result.to_string().contains("private-credential"));
    }
}
struct Waiting {
    started: Arc<tokio::sync::Notify>,
    token: Arc<Mutex<Option<ExtensionCancellation>>>,
}
#[async_trait]
impl ToolHook for Waiting {
    async fn before_tool(
        &self,
        _: ToolHookInput<'_>,
        cancel: ExtensionCancellation,
    ) -> Result<ToolHookDecision> {
        *self.token.lock().unwrap() = Some(cancel.clone());
        self.started.notify_one();
        cancel.cancelled().await;
        Ok(ToolHookDecision::Continue)
    }
}
#[tokio::test]
async fn cancelling_tool_hooks_cancels_their_child_and_closes_the_journal_without_execution() {
    let root = tempfile::tempdir().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let token = Arc::new(Mutex::new(None));
    let state = Arc::new(ToolState::default());
    let mut extension = Extension::new("waiting", "1").unwrap();
    extension
        .tool(Arc::new(Write(state.clone())))
        .unwrap()
        .tool_hook(
            HookSpec::new("wait"),
            Arc::new(Waiting {
                started: started.clone(),
                token: token.clone(),
            }),
        )
        .unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(Provider {
            tools: true,
            ..Default::default()
        })
        .workspace(root.path())
        .extensions(set(extension))
        .build()
        .unwrap(),
        AgentConfig::new("m"),
    );
    let repo = Arc::new(MemorySessionRepository::new());
    let saved = repo.clone();
    let context = TurnExecutionContext::new();
    let worker = context.clone();
    let owner = tokio::spawn(async move {
        agent
            .run_with_session_outcome(
                saved.as_ref(),
                SessionSelection::New,
                "test",
                Arc::new(()),
                worker,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    context.cancel("stop");
    let result = owner.await.unwrap();
    assert!(matches!(result, AgentTurnOutcome::Aborted(_)));
    assert!(token.lock().unwrap().as_ref().unwrap().is_cancelled());
    assert_eq!(state.prepared.load(Ordering::SeqCst), 0);
    assert!(
        repo.open(&result.session().unwrap().id)
            .unwrap()
            .unfinished_turn()
            .is_none()
    );
}
struct Command(u8, Arc<AtomicUsize>);
#[async_trait]
impl ExtensionCommand for Command {
    fn specification(&self) -> CommandSpec {
        CommandSpec {
            name: if self.0 == 3 { "exit" } else { "plan" }.into(),
            description: "plan a task".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn execute(
        &self,
        request: CommandRequest<'_>,
        _: ExtensionCancellation,
    ) -> Result<CommandOutput> {
        self.1.fetch_add(1, Ordering::SeqCst);
        assert!(request.context.workspace.is_absolute());
        match self.0 {
            0 => Ok(CommandOutput::Submit("Plan without modifying files".into())),
            1 => std::future::pending().await,
            2 => Ok(CommandOutput::Notice("x".repeat(65536))),
            4 => Ok(CommandOutput::Submit("x".repeat(2 * 1024 * 1024).into())),
            _ => Ok(CommandOutput::Notice("notice".into())),
        }
    }
}
#[tokio::test(start_paused = true)]
async fn commands_have_explicit_host_delivery_timeouts_reservations_and_bounded_payloads() {
    let root = tempfile::tempdir().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    for mode in [0, 1, 2, 4] {
        let mut extension = Extension::new("commands", "1").unwrap();
        extension
            .command(
                Arc::new(Command(mode, count.clone())),
                Duration::from_millis(1),
            )
            .unwrap();
        let installed = set(extension);
        let result = installed
            .execute_command(
                "plan",
                CommandRequest {
                    context: CommandContext {
                        workspace: root.path(),
                        session: None,
                    },
                    arguments: &json!({}),
                },
                &TurnExecutionContext::new(),
            )
            .await;
        if mode == 0 {
            assert!(
                matches!(result.unwrap(),CommandOutput::Submit(input) if input.message().text_content().contains("Plan"))
            );
        } else {
            assert!(result.is_err());
        }
        let before = count.load(Ordering::SeqCst);
        assert!(
            installed
                .execute_command(
                    "plan",
                    CommandRequest {
                        context: CommandContext {
                            workspace: root.path(),
                            session: None
                        },
                        arguments: &json!({"oversize":"x".repeat(65536)})
                    },
                    &TurnExecutionContext::new()
                )
                .await
                .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), before);
    }
    let mut registry = ExtensionRegistry::new();
    registry.reserve_command("exit").unwrap();
    let mut ext = Extension::new("collision", "1").unwrap();
    ext.command(Arc::new(Command(3, count)), Duration::from_secs(1))
        .unwrap();
    assert!(registry.install(ext).is_err());
}
struct Observe(&'static str, Arc<Mutex<Vec<String>>>);
impl AgentEventHandler for Observe {
    fn on_agent_started(&self, event: aicoder_core::events::AgentStartedEvent) {
        self.1
            .lock()
            .unwrap()
            .push(format!("{}:{}", self.0, event.meta.sequence));
    }
}
#[tokio::test]
async fn typed_observers_are_ordered_after_the_host_without_duplicate_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let mut extension = Extension::new("observation", "1").unwrap();
    extension
        .observer("late", 20, Arc::new(Observe("late", seen.clone())))
        .unwrap()
        .observer("early", -1, Arc::new(Observe("early", seen.clone())))
        .unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(Provider::default())
            .workspace(root.path())
            .extensions(set(extension))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let result = agent
        .run("test", Arc::new(Observe("host", seen.clone())))
        .await
        .unwrap();
    assert!(
        !result
            .execution_result
            .progress
            .event_delivery
            .has_failures()
    );
    assert_eq!(*seen.lock().unwrap(), vec!["host:1", "early:1", "late:1"]);
}
#[tokio::test]
async fn panicking_observers_report_delivery_failure_without_changing_execution() {
    let root = tempfile::tempdir().unwrap();
    let mut extension = Extension::new("observation", "1").unwrap();
    extension
        .observer(
            "panic",
            0,
            Arc::new(|_: &AgentRawEventEnvelope| panic!("fixture observer panic")),
        )
        .unwrap();
    let result = Agent::new(
        TurnExecutor::builder(Provider::default())
            .workspace(root.path())
            .extensions(set(extension))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    )
    .run("test", Arc::new(()))
    .await
    .unwrap();
    assert!(
        result
            .execution_result
            .progress
            .event_delivery
            .observers
            .iter()
            .any(|o| o.status == events::EventDeliveryStatus::HandlerPanicked)
    );
    assert_eq!(result.execution_result.final_message.text_content(), "done");
}
struct Keep;
impl context_policy::ContextPolicy for Keep {
    fn name(&self) -> &'static str {
        "extension-keep"
    }
    fn decide(
        &self,
        _: &context_policy::ContextPolicyInput<'_>,
    ) -> Result<context_policy::ContextDecision> {
        Ok(context_policy::ContextDecision::Keep)
    }
}
#[tokio::test]
async fn host_selected_registered_policy_cannot_bypass_known_windows() {
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(Provider::default());
    let mut extension = Extension::new("policy", "1").unwrap();
    extension
        .provider("chosen", provider.clone())
        .unwrap()
        .context_policy("keep", Arc::new(Keep))
        .unwrap();
    let installed = set(extension);
    let executor = TurnExecutor::builder_from_shared(installed.provider("chosen").unwrap())
        .workspace(root.path())
        .extensions(installed.clone())
        .context_policy(installed.context_policy("keep").unwrap())
        .build()
        .unwrap();
    let mut request = ModelRequest::new(
        "small",
        vec![Message::text(MessageRole::User, "x".repeat(2048))],
    );
    request.options.max_output_tokens = Some(1);
    assert!(executor.run(request).await.is_err());
    assert!(provider.requests.lock().unwrap().is_empty());
    assert!(installed.provider("unselected").is_none());
}

#[tokio::test]
async fn extensions_preserve_projected_recovery_system_markers_outside_the_declared_prefix() {
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(Provider::default());
    let mut extension = Extension::new("context", "1").unwrap();
    extension
        .context_hook(
            HookSpec::new("add"),
            Arc::new(Append("additional", Arc::new(Mutex::new(vec![])))),
        )
        .unwrap();
    let executor = TurnExecutor::builder_from_shared(provider.clone())
        .workspace(root.path())
        .extensions(set(extension))
        .build()
        .unwrap();
    let canonical = vec![
        Message::text(MessageRole::System, "host prefix"),
        Message::text(MessageRole::User, "retained user"),
    ];
    let mut projected = canonical.clone();
    projected.insert(
        1,
        Message::text(MessageRole::System, "derived recovery marker"),
    );
    let input =
        ExecutionInput::projected(ModelRequest::new("m", projected), canonical.clone(), 1).unwrap();
    let result = executor.run_input(input).await.unwrap();
    assert_eq!(
        provider.requests.lock().unwrap()[0]
            .messages
            .iter()
            .map(Message::text_content)
            .collect::<Vec<_>>(),
        vec![
            "host prefix",
            "additional",
            "derived recovery marker",
            "retained user"
        ]
    );
    assert!(
        !result
            .messages
            .iter()
            .any(|m| m.text_content() == "derived recovery marker")
    );
}

#[async_trait]
impl ContextHook for Waiting {
    async fn context(
        &self,
        _: ContextHookInput<'_>,
        cancel: ExtensionCancellation,
    ) -> Result<Option<String>> {
        *self.token.lock().unwrap() = Some(cancel.clone());
        self.started.notify_one();
        cancel.cancelled().await;
        Ok(None)
    }
}
#[async_trait]
impl ExtensionCommand for Waiting {
    fn specification(&self) -> CommandSpec {
        CommandSpec {
            name: "waiting".into(),
            description: "wait".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn execute(
        &self,
        _: CommandRequest<'_>,
        cancel: ExtensionCancellation,
    ) -> Result<CommandOutput> {
        *self.token.lock().unwrap() = Some(cancel.clone());
        self.started.notify_one();
        cancel.cancelled().await;
        Ok(CommandOutput::Notice("late result".into()))
    }
}
#[tokio::test]
async fn context_and_command_cancellation_drop_children_and_cannot_deliver_late_results() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().to_path_buf();
    let started = Arc::new(tokio::sync::Notify::new());
    let token = Arc::new(Mutex::new(None));
    let mut extension = Extension::new("waiting", "1").unwrap();
    extension
        .context_hook(
            HookSpec::new("context"),
            Arc::new(Waiting {
                started: started.clone(),
                token: token.clone(),
            }),
        )
        .unwrap();
    extension
        .command(
            Arc::new(Waiting {
                started: started.clone(),
                token: token.clone(),
            }),
            Duration::from_secs(2),
        )
        .unwrap();
    let installed = set(extension);
    for command in [false, true] {
        let context = TurnExecutionContext::new();
        let worker = context.clone();
        let snapshot = installed.clone();
        let path = workspace.clone();
        let owner = tokio::spawn(async move {
            if command {
                snapshot
                    .execute_command(
                        "waiting",
                        CommandRequest {
                            context: CommandContext {
                                workspace: &path,
                                session: None,
                            },
                            arguments: &json!({}),
                        },
                        &worker,
                    )
                    .await
                    .is_err()
            } else {
                let agent = Agent::new(
                    TurnExecutor::builder(Provider::default())
                        .workspace(path)
                        .extensions(snapshot)
                        .build()
                        .unwrap(),
                    AgentConfig::new("m"),
                );
                matches!(
                    agent.run_outcome("test", Arc::new(()), worker).await,
                    AgentTurnOutcome::Aborted(_)
                )
            }
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        context.cancel("stop");
        assert!(owner.await.unwrap());
        assert!(token.lock().unwrap().as_ref().unwrap().is_cancelled());
    }
}
#[tokio::test]
async fn blocked_observer_has_bounded_drain_and_explicit_failure_without_stopping_the_owner() {
    let root = tempfile::tempdir().unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let (done, finished) = std::sync::mpsc::channel();
    let wait = Mutex::new(wait);
    let first = AtomicBool::new(true);
    let mut extension = Extension::new("observation", "1").unwrap();
    extension
        .observer(
            "blocked",
            0,
            Arc::new(move |_: &AgentRawEventEnvelope| {
                if first.swap(false, Ordering::SeqCst) {
                    wait.lock().unwrap().recv().unwrap();
                    let _ = done.send(());
                }
            }),
        )
        .unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(Provider::default())
            .workspace(root.path())
            .extensions(set(extension))
            .config(TurnExecutionConfig {
                events: events::EventDeliveryConfig {
                    callback_drain_timeout: Duration::from_millis(10),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let result = agent.run("test", Arc::new(())).await.unwrap();
    assert!(
        result
            .execution_result
            .progress
            .event_delivery
            .observers
            .iter()
            .any(|o| o.status == events::EventDeliveryStatus::CallbackTimedOut)
    );
    release.send(()).unwrap();
    tokio::task::spawn_blocking(move || finished.recv_timeout(Duration::from_secs(2)))
        .await
        .unwrap()
        .unwrap();
}
