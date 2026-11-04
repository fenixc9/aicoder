use aicoder_core::{artifacts::*, extensions::*, model::*, session::*, tools::*, *};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
#[derive(Default)]
struct State {
    prepared: Mutex<Vec<Value>>,
    approved: Mutex<Vec<Value>>,
    executed: AtomicUsize,
    order: Mutex<Vec<String>>,
}
struct Provider {
    requests: Mutex<Vec<ModelRequest>>,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", model);
        s.capabilities.tools = true;
        s.context_tokens = Some(65536);
        Ok(s)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let tool = request.messages.last().unwrap().role == MessageRole::User;
        self.requests.lock().unwrap().push(request.clone());
        let message = if tool {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "c".into(),
                        name: "write".into(),
                        arguments: json!({"value":"original"}),
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
            stop_reason: if tool {
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
struct Write(Arc<State>, bool, Option<ArtifactRef>);
#[async_trait]
impl ExecutableTool for Write {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn prepare(
        &self,
        _: &ToolContext,
        args: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        self.0.order.lock().unwrap().push("prepare".into());
        self.0.prepared.lock().unwrap().push(args.clone());
        Ok(Some(Box::new(Plan(
            self.0.clone(),
            args,
            self.1,
            self.2.clone(),
        ))))
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        panic!("prepared plan required")
    }
}
struct Plan(Arc<State>, Value, bool, Option<ArtifactRef>);
#[async_trait]
impl PreparedTool for Plan {
    fn preview(&self) -> ToolPreview {
        ToolPreview {
            summary: "owned plan".into(),
            body: self.1.to_string(),
            format: "json".into(),
            truncated: false,
        }
    }
    async fn execute(self: Box<Self>, _: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        self.0.executed.fetch_add(1, Ordering::SeqCst);
        self.0.order.lock().unwrap().push("execute".into());
        if self.2 {
            return Err(ToolFailure::new("fixture_failure", "private-result")
                .with_details(json!({"operation_completed":true})));
        }
        let mut result =
            ToolSuccess::new(json!({"value":self.1["value"],"secret":"private-result"}));
        if let Some(r) = self.3 {
            result.artifacts.push(r);
        }
        Ok(result)
    }
}
struct Approval(Arc<State>);
#[async_trait]
impl ApprovalHandler for Approval {
    async fn approve(&self, call: &ToolInvocation) -> Result<bool> {
        assert_eq!(
            call.preview.as_ref().unwrap().body,
            call.arguments.to_string()
        );
        self.0.approved.lock().unwrap().push(call.arguments.clone());
        self.0.order.lock().unwrap().push("approve".into());
        Ok(true)
    }
}
struct Change(Arc<State>, &'static str);
#[async_trait]
impl ToolCallHook for Change {
    async fn before_call(
        &self,
        input: ToolCallHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<ToolCallEdit> {
        assert_eq!(input.original.arguments["value"], "original");
        self.0.order.lock().unwrap().push(self.1.into());
        let value = format!(
            "{}-{}",
            input.call.arguments["value"].as_str().unwrap(),
            self.1
        );
        Ok(ToolCallEdit::Arguments(json!({"value":value})))
    }
}
struct Guard(Arc<State>);
#[async_trait]
impl ToolHook for Guard {
    async fn before_tool(
        &self,
        input: ToolHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<ToolHookDecision> {
        assert_eq!(input.call.arguments["value"], "original-first-second");
        self.0.order.lock().unwrap().push("guard".into());
        Ok(ToolHookDecision::Continue)
    }
}
struct Redact(Arc<State>);
#[async_trait]
impl ToolResultHook for Redact {
    async fn after_tool(
        &self,
        input: ToolResultHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<ToolResultEdit>> {
        self.0.order.lock().unwrap().push("redact".into());
        assert!(input.envelope.to_string().contains("private-result"));
        assert_eq!(input.operation_completed, Some(true));
        Ok(Some(ToolResultEdit {
            is_error: false,
            payload: json!({"value":"redacted"}),
            artifacts: vec![],
        }))
    }
}
struct Annotate(Arc<State>);
#[async_trait]
impl ToolResultHook for Annotate {
    async fn after_tool(
        &self,
        input: ToolResultHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<ToolResultEdit>> {
        self.0.order.lock().unwrap().push("annotate".into());
        assert!(!input.envelope.to_string().contains("private-result"));
        Ok(Some(ToolResultEdit {
            is_error: false,
            payload: json!({"value":input.envelope["output"]["value"],"annotated":true}),
            artifacts: vec![],
        }))
    }
}
fn set(extension: Extension) -> Arc<ExtensionSet> {
    let mut r = ExtensionRegistry::new();
    r.install(extension).unwrap();
    Arc::new(r.finish())
}
fn provider() -> Arc<Provider> {
    Arc::new(Provider {
        requests: Mutex::new(vec![]),
    })
}
fn registry(state: Arc<State>, failed: bool, artifact: Option<ArtifactRef>) -> ToolRegistry {
    let mut r = ToolRegistry::default();
    r.register(Write(state, failed, artifact)).unwrap();
    r
}
fn records(session: &Session) -> Vec<Value> {
    session
        .entries()
        .iter()
        .filter_map(|e| match &e.data {
            SessionEntryData::Extension { namespace, data }
                if namespace == INTERCEPTION_NAMESPACE =>
            {
                Some(data.clone())
            }
            _ => None,
        })
        .collect()
}
#[tokio::test]
async fn arguments_chain_precedes_guards_and_owned_approval_while_delivered_results_chain_without_raw_secrets()
 {
    for failed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(State::default());
        let model = provider();
        let mut e = Extension::new("intercept", "1").unwrap();
        e.tool_call_hook(
            HookSpec {
                priority: 20,
                ..HookSpec::new("second")
            },
            Arc::new(Change(state.clone(), "second")),
        )
        .unwrap();
        e.tool_call_hook(
            HookSpec {
                priority: -1,
                ..HookSpec::new("first")
            },
            Arc::new(Change(state.clone(), "first")),
        )
        .unwrap();
        e.tool_hook(HookSpec::new("guard"), Arc::new(Guard(state.clone())))
            .unwrap();
        e.tool_result_hook(HookSpec::new("redact"), Arc::new(Redact(state.clone())))
            .unwrap();
        e.tool_result_hook(HookSpec::new("annotate"), Arc::new(Annotate(state.clone())))
            .unwrap();
        let executor = TurnExecutor::builder_from_shared(model.clone())
            .workspace(root.path())
            .registry(registry(state.clone(), failed, None))
            .approval(Approval(state.clone()))
            .extensions(set(e))
            .build()
            .unwrap();
        let repo = MemorySessionRepository::new();
        let session = repo.create(root.path()).unwrap();
        let turn = Agent::new(executor, AgentConfig::new("fixture"))
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
        assert_eq!(state.executed.load(Ordering::SeqCst), 1);
        assert_eq!(
            *state.order.lock().unwrap(),
            vec![
                "first", "second", "guard", "prepare", "approve", "execute", "redact", "annotate"
            ]
        );
        assert_eq!(
            state.prepared.lock().unwrap()[0],
            state.approved.lock().unwrap()[0]
        );
        let saved = repo.open(&session.metadata().id).unwrap();
        let messages = saved
            .messages()
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>();
        let original = messages
            .iter()
            .flat_map(Message::tool_calls)
            .next()
            .unwrap();
        assert_eq!(original.arguments["value"], "original");
        let result = turn
            .execution_result
            .messages
            .iter()
            .find_map(Message::tool_result)
            .unwrap();
        assert!(!result.2);
        assert_eq!(result.1["interception"]["arguments_changed"], true);
        assert_eq!(
            result.1["output"],
            json!({"value":"redacted","annotated":true})
        );
        assert_eq!(
            result.1["interception"]["execution"],
            if failed { "failed" } else { "succeeded" }
        );
        let records = records(&saved);
        assert_eq!(records.len(), 4);
        assert_eq!(records[0]["arguments"]["value"], "original-first");
        assert_eq!(records[1]["arguments"]["value"], "original-first-second");
        assert_eq!(records[2]["kind"], "result_pending");
        assert_eq!(records[3]["changes"].as_array().unwrap().len(), 2);
        assert!(
            !serde_json::to_string(saved.entries())
                .unwrap()
                .contains("private-result")
        );
        assert!(
            !serde_json::to_string(&model.requests.lock().unwrap()[1])
                .unwrap()
                .contains("private-result")
        );
    }
}
struct BadJournal;
#[async_trait]
impl RunJournal for BadJournal {
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
struct Pass;
#[async_trait]
impl ToolResultHook for Pass {
    async fn after_tool(
        &self,
        _: ToolResultHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<ToolResultEdit>> {
        Ok(None)
    }
}
#[tokio::test]
async fn unsupported_interception_journals_stop_before_execution() {
    for arguments in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(State::default());
        let mut e = Extension::new("receipt", "1").unwrap();
        if arguments {
            e.tool_call_hook(
                HookSpec::new("change"),
                Arc::new(Change(state.clone(), "change")),
            )
            .unwrap();
        } else {
            e.tool_result_hook(HookSpec::new("observe"), Arc::new(Pass))
                .unwrap();
        }
        let executor = TurnExecutor::builder_from_shared(provider())
            .workspace(root.path())
            .registry(registry(state.clone(), false, None))
            .approval(AllowAllApproval)
            .extensions(set(e))
            .build()
            .unwrap();
        let outcome = executor
            .run_with_journal(
                ModelRequest::new("fixture", vec![Message::text(MessageRole::User, "run")]),
                Arc::new(()),
                TurnExecutionContext::new(),
                &BadJournal,
            )
            .await;
        assert!(
            matches!(outcome,RunOutcome::Interrupted(ref i)if matches!(&i.stop,RunStop::Failed(f)if f.stage==RunStage::Persistence)),
            "{outcome:?}"
        );
        assert_eq!(state.executed.load(Ordering::SeqCst), 0);
        if arguments {
            assert!(state.prepared.lock().unwrap().is_empty());
        }
    }
}
struct ArtifactEdit {
    supplied: ArtifactRef,
    drop_first: bool,
}
#[async_trait]
impl ToolResultHook for ArtifactEdit {
    async fn after_tool(
        &self,
        _input: ToolResultHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<ToolResultEdit>> {
        Ok(Some(ToolResultEdit {
            is_error: false,
            payload: json!("replaced"),
            artifacts: if self.drop_first {
                vec![]
            } else {
                vec![self.supplied.clone()]
            },
        }))
    }
}
#[tokio::test]
async fn result_hooks_can_filter_but_cannot_forge_modify_or_restore_artifact_exposure() {
    let reference = ArtifactRef {
        version: 1,
        id: "00000000-0000-0000-0000-000000000001".into(),
        media_type: "image/png".into(),
        bytes: 3,
        complete: true,
    };
    for mode in ["retain", "filter", "forge", "alter", "restore"] {
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(State::default());
        let mut e = Extension::new("media", "1").unwrap();
        let mut supplied = reference.clone();
        if mode == "forge" {
            supplied.id = "00000000-0000-0000-0000-000000000002".into();
        }
        if mode == "alter" {
            supplied.complete = false;
        }
        e.tool_result_hook(
            HookSpec::new("first"),
            Arc::new(ArtifactEdit {
                supplied: supplied.clone(),
                drop_first: mode == "filter" || mode == "restore",
            }),
        )
        .unwrap();
        if mode == "restore" {
            e.tool_result_hook(
                HookSpec::new("restore"),
                Arc::new(ArtifactEdit {
                    supplied,
                    drop_first: false,
                }),
            )
            .unwrap();
        }
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(provider())
                .workspace(root.path())
                .registry(registry(state.clone(), false, Some(reference.clone())))
                .approval(AllowAllApproval)
                .extensions(set(e))
                .build()
                .unwrap(),
            AgentConfig::new("fixture"),
        );
        let turn = agent.run("run", Arc::new(())).await.unwrap();
        assert_eq!(state.executed.load(Ordering::SeqCst), 1);
        let result = turn
            .execution_result
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .unwrap();
        let (_, value, error) = result.tool_result().unwrap();
        assert_eq!(error, matches!(mode, "forge" | "alter" | "restore"));
        assert_eq!(
            result
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::Image { .. }))
                .count(),
            usize::from(mode == "retain")
        );
        if error {
            assert_eq!(value["error"]["details"]["operation_completed"], true);
            assert_eq!(value["interception"]["delivery_complete"], false);
        }
    }
}
struct Slow {
    started: Arc<tokio::sync::Notify>,
    cancelled: Arc<tokio::sync::Notify>,
}
impl Slow {
    async fn wait(&self, cancellation: ExtensionCancellation) {
        let signal = self.cancelled.clone();
        tokio::spawn(async move {
            cancellation.cancelled().await;
            signal.notify_one();
        });
        self.started.notify_one();
        std::future::pending::<()>().await;
    }
}
#[async_trait]
impl ToolCallHook for Slow {
    async fn before_call(
        &self,
        _: ToolCallHookInput<'_>,
        c: ExtensionCancellation,
    ) -> Result<ToolCallEdit> {
        self.wait(c).await;
        Ok(ToolCallEdit::Continue)
    }
}
#[async_trait]
impl ToolResultHook for Slow {
    async fn after_tool(
        &self,
        _: ToolResultHookInput<'_>,
        c: ExtensionCancellation,
    ) -> Result<Option<ToolResultEdit>> {
        self.wait(c).await;
        Ok(None)
    }
}
#[tokio::test]
async fn timed_out_callbacks_cancel_children_and_completed_tools_are_not_repeated() {
    for after in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(State::default());
        let cancelled = Arc::new(tokio::sync::Notify::new());
        let hook = Arc::new(Slow {
            started: Arc::new(tokio::sync::Notify::new()),
            cancelled: cancelled.clone(),
        });
        let mut e = Extension::new("timeout", "1").unwrap();
        let spec = HookSpec {
            timeout: Duration::from_millis(10),
            ..HookSpec::new("slow")
        };
        if after {
            e.tool_result_hook(spec, hook).unwrap();
        } else {
            e.tool_call_hook(spec, hook).unwrap();
        }
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(provider())
                .workspace(root.path())
                .registry(registry(state.clone(), false, None))
                .approval(AllowAllApproval)
                .extensions(set(e))
                .build()
                .unwrap(),
            AgentConfig::new("fixture"),
        );
        let turn = agent.run("run", Arc::new(())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), cancelled.notified())
            .await
            .unwrap();
        assert_eq!(state.executed.load(Ordering::SeqCst), usize::from(after));
        let result = turn
            .execution_result
            .messages
            .iter()
            .find_map(Message::tool_result)
            .unwrap();
        assert!(result.2);
        assert!(!result.1.to_string().contains("private-result"));
        if after {
            assert_eq!(result.1["error"]["details"]["operation_completed"], true);
        }
    }
}
#[tokio::test]
async fn parent_cancellation_during_result_hook_preserves_completed_effect_and_no_late_callback() {
    let root = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let started = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let mut e = Extension::new("cancel", "1").unwrap();
    e.tool_result_hook(
        HookSpec::new("slow"),
        Arc::new(Slow {
            started: started.clone(),
            cancelled: cancelled.clone(),
        }),
    )
    .unwrap();
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider())
            .workspace(root.path())
            .registry(registry(state.clone(), false, None))
            .approval(AllowAllApproval)
            .extensions(set(e))
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let context = TurnExecutionContext::new();
    let worker = context.clone();
    let run = tokio::spawn(async move { agent.run_outcome("run", Arc::new(()), worker).await });
    started.notified().await;
    context.cancel("cancel result transformation");
    let outcome = run.await.unwrap();
    assert!(matches!(outcome, AgentTurnOutcome::Aborted(_)));
    assert_eq!(state.executed.load(Ordering::SeqCst), 1);
    tokio::time::timeout(Duration::from_secs(1), cancelled.notified())
        .await
        .unwrap();
}

struct PendingJournal {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl RunJournal for PendingJournal {
    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn record_interception(&self, record: &InterceptionRecord) -> Result<()> {
        if matches!(record, InterceptionRecord::ResultPending { .. }) {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn revocation_or_cancellation_while_pending_receipt_is_written_prevents_execution() {
    for cancel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(State::default());
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut e = Extension::new("pending", "1").unwrap();
        e.tool_result_hook(HookSpec::new("result"), Arc::new(Pass))
            .unwrap();
        let approval = ApprovalSessions::default()
            .session(root.path(), "s")
            .unwrap();
        let context = TurnExecutionContext::new().with_approval_session(approval.clone());
        let worker = context.clone();
        let executor = TurnExecutor::builder_from_shared(provider())
            .workspace(root.path())
            .registry(registry(state.clone(), false, None))
            .approval(AllowAllApproval)
            .extensions(set(e))
            .build()
            .unwrap();
        let journal = PendingJournal {
            started: started.clone(),
            release: release.clone(),
        };
        let run = tokio::spawn(async move {
            executor
                .run_with_journal(
                    ModelRequest::new("fixture", vec![Message::text(MessageRole::User, "run")]),
                    Arc::new(()),
                    worker,
                    &journal,
                )
                .await
        });
        started.notified().await;
        if cancel {
            context.cancel("cancel pending interception record");
        } else {
            approval.revoke_all();
            release.notify_one();
        }
        let outcome = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.executed.load(Ordering::SeqCst), 0);
        if cancel {
            assert!(
                matches!(outcome,RunOutcome::Interrupted(ref turn)if matches!(turn.stop,RunStop::Cancelled{..}))
            );
        } else {
            let result = outcome
                .progress()
                .messages
                .iter()
                .find_map(Message::tool_result)
                .unwrap();
            assert!(result.2);
            assert_eq!(result.1["error"]["code"], "approval_revoked");
        }
    }
}
#[tokio::test]
async fn restored_interception_records_do_not_reinstall_argument_or_result_authority() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let session = repo.create(root.path()).unwrap();
    let state = Arc::new(State::default());
    let mut e = Extension::new("recorded", "1").unwrap();
    e.tool_call_hook(
        HookSpec::new("change"),
        Arc::new(Change(state.clone(), "first")),
    )
    .unwrap();
    e.tool_result_hook(HookSpec::new("redact"), Arc::new(Redact(state.clone())))
        .unwrap();
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider())
            .workspace(root.path())
            .registry(registry(state.clone(), false, None))
            .approval(AllowAllApproval)
            .extensions(set(e))
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
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
    let before = records(&repo.open(&session.metadata().id).unwrap()).len();
    let fresh = Agent::new(
        TurnExecutor::builder_from_shared(provider())
            .workspace(root.path())
            .registry(registry(state.clone(), false, None))
            .approval(AllowAllApproval)
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let turn = fresh
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
    let prepared = state.prepared.lock().unwrap();
    assert_eq!(prepared[0]["value"], "original-first");
    assert_eq!(prepared[1]["value"], "original");
    assert_eq!(
        records(&repo.open(&session.metadata().id).unwrap()).len(),
        before
    );
    assert!(turn.execution_result.messages.last().unwrap().role == MessageRole::Assistant);
}

struct BadEdit(&'static str);
#[async_trait]
impl ToolCallHook for BadEdit {
    async fn before_call(
        &self,
        _: ToolCallHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<ToolCallEdit> {
        Ok(match self.0 {
            "deny" => ToolCallEdit::Deny {
                reason: "host denial".into(),
            },
            "array" => ToolCallEdit::Arguments(json!([])),
            _ => ToolCallEdit::Arguments(json!({"large":"x".repeat(65537)})),
        })
    }
}
#[async_trait]
impl ToolResultHook for BadEdit {
    async fn after_tool(
        &self,
        _: ToolResultHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<ToolResultEdit>> {
        if self.0 == "error" {
            anyhow::bail!("native-private-error");
        }
        Ok(Some(ToolResultEdit {
            is_error: self.0 == "invalid_error",
            payload: if self.0 == "invalid_error" {
                json!({"message":"missing code"})
            } else {
                json!("x".repeat(65537))
            },
            artifacts: vec![],
        }))
    }
}
#[tokio::test]
async fn bounded_edits_fail_closed_and_hidden_tools_never_reach_interceptors() {
    for mode in [
        "deny",
        "array",
        "large",
        "hidden",
        "error",
        "invalid_error",
        "long_result",
    ] {
        let after = matches!(mode, "error" | "invalid_error" | "long_result");
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(State::default());
        let mut e = Extension::new("bad", "1").unwrap();
        if after {
            e.tool_result_hook(HookSpec::new("edit"), Arc::new(BadEdit(mode)))
                .unwrap();
        } else {
            e.tool_call_hook(HookSpec::new("edit"), Arc::new(BadEdit(mode)))
                .unwrap();
        }
        let mut builder = TurnExecutor::builder_from_shared(provider())
            .workspace(root.path())
            .registry(registry(state.clone(), false, None))
            .approval(AllowAllApproval)
            .extensions(set(e));
        if mode == "hidden" {
            builder = builder.model_tools(vec![]);
        }
        let turn = Agent::new(builder.build().unwrap(), AgentConfig::new("fixture"))
            .run("run", Arc::new(()))
            .await
            .unwrap();
        assert_eq!(state.executed.load(Ordering::SeqCst), usize::from(after));
        if !after {
            assert!(state.prepared.lock().unwrap().is_empty());
        }
        let result = turn
            .execution_result
            .messages
            .iter()
            .find_map(Message::tool_result)
            .unwrap();
        assert!(result.2);
        assert!(!result.1.to_string().contains("native-private-error"));
        if after {
            assert_eq!(result.1["error"]["details"]["operation_completed"], true);
        }
        if mode == "hidden" {
            assert_eq!(result.1["error"]["code"], "tool_not_exposed");
        }
    }
}

struct Uncertain(Arc<AtomicUsize>, Option<bool>);
#[async_trait]
impl ExecutableTool for Uncertain {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(
            ToolFailure::new("uncertain", "operation completion is unknown")
                .with_details(json!({"operation_completed":self.1,"large":"x".repeat(65537)})),
        )
    }
}
#[tokio::test]
async fn result_interception_failure_cannot_turn_unknown_operation_completion_into_false() {
    for completed in [None, Some(true)] {
        let root = tempfile::tempdir().unwrap();
        let executed = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::default();
        registry
            .register(Uncertain(executed.clone(), completed))
            .unwrap();
        let mut e = Extension::new("uncertain", "1").unwrap();
        e.tool_result_hook(HookSpec::new("error"), Arc::new(BadEdit("error")))
            .unwrap();
        let agent = Agent::new(
            TurnExecutor::builder_from_shared(provider())
                .workspace(root.path())
                .registry(registry)
                .approval(AllowAllApproval)
                .extensions(set(e))
                .build()
                .unwrap(),
            AgentConfig::new("fixture"),
        );
        let turn = agent.run("run", Arc::new(())).await.unwrap();
        assert_eq!(executed.load(Ordering::SeqCst), 1);
        let result = turn
            .execution_result
            .messages
            .iter()
            .find_map(Message::tool_result)
            .unwrap();
        assert!(result.2);
        assert_eq!(
            result.1["error"]["details"]["operation_completed"],
            json!(completed)
        );
        assert_eq!(
            result.1["error"]["details"]["completion_uncertain"],
            completed.is_none()
        );
        assert_eq!(result.1["interception"]["execution"], "failed");
    }
}

struct TimeoutResult;
#[async_trait]
impl ExecutableTool for TimeoutResult {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        Err(ToolFailure::new("timeout", "fixture timeout"))
    }
}
#[derive(Default)]
struct Outcomes(Mutex<Vec<events::ToolExecutionOutcome>>);
impl AgentEventHandler for Outcomes {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        if let AgentRawEvent::ToolExecutionEnded { outcome, .. } = &event.event {
            self.0.lock().unwrap().push(outcome.as_ref().clone());
        }
    }
}
#[tokio::test]
async fn unchanged_result_callbacks_preserve_the_original_timeout_event_category() {
    let root = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::default();
    registry.register(TimeoutResult).unwrap();
    let mut e = Extension::new("observe", "1").unwrap();
    e.tool_result_hook(HookSpec::new("pass"), Arc::new(Pass))
        .unwrap();
    let events = Arc::new(Outcomes::default());
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider())
            .workspace(root.path())
            .registry(registry)
            .extensions(set(e))
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let turn = agent.run("run", events.clone()).await.unwrap();
    let outcomes = events.0.lock().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes[0],
        events::ToolExecutionOutcome::TimedOut
    ));
    let result = turn
        .execution_result
        .messages
        .iter()
        .find_map(Message::tool_result)
        .unwrap();
    assert_eq!(result.1["interception"]["execution"], "timed_out");
    assert!(result.1["interception"]["operation_completed"].is_null());
}
