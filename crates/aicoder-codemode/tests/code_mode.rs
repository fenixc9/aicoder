#![cfg(feature = "engine")]
use aicoder_codemode::{CodeModeConfig, CodeModeTool};
use aicoder_core::{composition::*, model::*, session::*, tools::*, *};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Echo(Arc<AtomicUsize>);
#[async_trait]
impl ExecutableTool for Echo {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo".into(),
            description: Some("Return structured input with a hidden large field".into()),
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(&self, _: &ToolContext, args: Value) -> Result<ToolSuccess, ToolFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess::new(
            json!({"value":args["value"],"private_large":"secret_data_".repeat(128)}),
        ))
    }
}
struct Mutate(Arc<AtomicUsize>);
struct Plan(Arc<AtomicUsize>, Value);
#[async_trait]
impl ExecutableTool for Mutate {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "mutate".into(),
            description: Some("Prepared mutation".into()),
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
        Ok(Some(Box::new(Plan(self.0.clone(), args))))
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        panic!("owned preparation must be used")
    }
}
#[async_trait]
impl PreparedTool for Plan {
    fn preview(&self) -> ToolPreview {
        ToolPreview {
            summary: "Captured mutation".into(),
            body: self.1.to_string(),
            format: "text".into(),
            truncated: false,
        }
    }
    async fn execute(self: Box<Self>, _: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess::new(self.1))
    }
}
struct Wait(Arc<tokio::sync::Notify>);
#[async_trait]
impl ExecutableTool for Wait {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "wait".into(),
            description: Some("Await cancellation".into()),
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        self.0.notify_one();
        std::future::pending().await
    }
}
struct Model {
    once: bool,
    trailing: Vec<ModelToolCall>,
    code: String,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.tools = true;
        spec.context_tokens = Some(65536);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let mut requests = self.requests.lock().unwrap();
        let first = requests.is_empty();
        requests.push(request.clone());
        drop(requests);
        let call =
            request.messages.last().unwrap().role == MessageRole::User && (!self.once || first);
        let message = if call {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "program".into(),
                        name: "codemode".into(),
                        arguments: json!({"code":self.code}),
                    },
                }],
            }
        } else {
            Message::text(MessageRole::Assistant, "done")
        };
        let mut message = message;
        if call {
            message.content.extend(
                self.trailing
                    .iter()
                    .cloned()
                    .map(|call| ContentBlock::ToolCall { call }),
            );
        }
        let response = ModelResponse {
            id: None,
            model: request.model,
            message,
            stop_reason: if call {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage {
                input_tokens: Some(5),
                output_tokens: Some(2),
                total_tokens: Some(7),
                ..Default::default()
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Approval {
    deny_mutation: bool,
    requests: Arc<Mutex<Vec<ToolInvocation>>>,
}
#[async_trait]
impl ApprovalHandler for Approval {
    async fn approve(&self, invocation: &ToolInvocation) -> anyhow::Result<bool> {
        self.requests.lock().unwrap().push(invocation.clone());
        Ok(!self.deny_mutation || invocation.name != "mutate")
    }
}
struct Host {
    waiting: Arc<tokio::sync::Notify>,
    agent: Agent,
    mode: Arc<CodeModeTool>,
    echo: Arc<AtomicUsize>,
    mutations: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    approvals: Arc<Mutex<Vec<ToolInvocation>>>,
}
fn host(root: &Path, code: &str, deny: bool, budget: RunBudget, config: CodeModeConfig) -> Host {
    let mode = Arc::new(CodeModeTool::new(config).unwrap());
    let echo = Arc::new(AtomicUsize::new(0));
    let mutations = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    let waiting = Arc::new(tokio::sync::Notify::new());
    registry.register(Wait(waiting.clone())).unwrap();
    registry.register(Echo(echo.clone())).unwrap();
    registry.register(Mutate(mutations.clone())).unwrap();
    registry.register_shared(mode.clone()).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let approvals = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        TurnExecutor::builder(Model {
            once: false,
            trailing: Vec::new(),
            code: code.into(),
            requests: requests.clone(),
        })
        .workspace(root)
        .registry(registry)
        .approval(Approval {
            deny_mutation: deny,
            requests: approvals.clone(),
        })
        .config(TurnExecutionConfig {
            budget,
            ..Default::default()
        })
        .build()
        .unwrap(),
        AgentConfig::new("offline"),
    );
    Host {
        waiting,
        agent,
        mode,
        echo,
        mutations,
        requests,
        approvals,
    }
}
fn program_result(result: &AgentTurnResult) -> Value {
    result
        .execution_result
        .messages
        .iter()
        .find_map(|m| {
            m.tool_result()
                .filter(|(id, _, _)| *id == "program")
                .map(|(_, v, _)| v.clone())
        })
        .unwrap()
}
#[tokio::test]
async fn real_javascript_parallel_calls_filter_model_output_and_charge_all_tool_calls() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        "const values = await Promise.allSettled([tools.echo({value:1}), tools.echo({value:2})]); text(values.map(v => v.value.value));",
        false,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    let result = host.agent.run("run", Arc::new(())).await.unwrap();
    let value = program_result(&result);
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(value["output"]["program"]["items"][0], json!([1, 2]));
    assert_eq!(result.execution_result.tool_calls, 3);
    assert_eq!(result.execution_result.usage.total_tokens, Some(14));
    assert_eq!(host.echo.load(Ordering::SeqCst), 2);
    assert_eq!(host.mode.active_workers(), 0);
    let requests = host.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        !serde_json::to_string(&requests[1])
            .unwrap()
            .contains("secret_data_")
    );
    assert_eq!(host.approvals.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn script_permission_is_not_leaf_mutation_permission_and_all_settled_receives_rejection() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        "const v=await Promise.allSettled([tools.mutate({value:3}),tools.echo({value:4})]); text(v.map(x=>x.status));",
        true,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    let result = host.agent.run("run", Arc::new(())).await.unwrap();
    let value = program_result(&result);
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(
        value["output"]["program"]["items"][0],
        json!(["rejected", "fulfilled"])
    );
    assert_eq!(host.mutations.load(Ordering::SeqCst), 0);
    let approvals = host.approvals.lock().unwrap();
    assert_eq!(
        approvals
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>(),
        ["codemode", "mutate"]
    );
    assert_eq!(approvals[1].preview.as_ref().unwrap().body, "{\"value\":3}");
}
#[tokio::test]
async fn actual_leaf_records_are_canonical_but_not_fabricated_parent_tool_messages() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        "text((await tools.echo({value:9})).value);",
        false,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    let repository = MemorySessionRepository::new();
    let result = host
        .agent
        .run_with_session(&repository, SessionSelection::New, "run", Arc::new(()))
        .await
        .unwrap();
    let session = repository.open(&result.session.unwrap().id).unwrap();
    let mut calls = 0;
    let mut results = 0;
    for entry in session.entries() {
        if let SessionEntryData::Extension { namespace, data } = &entry.data
            && namespace == COMPOSITION_NAMESPACE
        {
            match serde_json::from_value::<CompositionRecord>(data.clone()).unwrap() {
                CompositionRecord::Call { .. } => calls += 1,
                CompositionRecord::Result { .. } => results += 1,
                _ => {}
            }
        }
    }
    assert_eq!((calls, results), (1, 1));
    let messages = session.model_messages().unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .count(),
        1
    );
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("secret_data_")
    );
}
#[tokio::test]
async fn parent_tool_budget_blocks_hidden_additional_calls_before_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        "text(await Promise.all([tools.echo({value:1}),tools.echo({value:2})]));",
        false,
        RunBudget {
            max_tool_calls: Some(2),
            ..Default::default()
        },
        CodeModeConfig::default(),
    );
    let outcome = host
        .agent
        .run_outcome("run", Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
    assert!(host.echo.load(Ordering::SeqCst) <= 1);
    assert_eq!(host.mode.active_workers(), 0);
}
#[tokio::test]
async fn throw_retains_partial_output_and_never_repeats_completed_mutation() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        "await tools.mutate({value:1}); text('before failure'); throw new Error('stop here');",
        false,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    let result = host.agent.run("run", Arc::new(())).await.unwrap();
    let value = program_result(&result);
    assert_eq!(value["ok"], false);
    assert!(
        value["error"]["details"]
            .to_string()
            .contains("before failure")
    );
    assert_eq!(host.mutations.load(Ordering::SeqCst), 1);
    assert_eq!(host.mode.active_workers(), 0);
}
#[tokio::test]
async fn branch_local_public_store_commits_only_success_and_never_restores_authority() {
    let root = tempfile::tempdir().unwrap();
    let repository = MemorySessionRepository::new();
    let first = host(
        root.path(),
        "store('counter',3); text(load('counter'));",
        false,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    let result = first
        .agent
        .run_with_session(&repository, SessionSelection::New, "run", Arc::new(()))
        .await
        .unwrap();
    assert_eq!(program_result(&result)["ok"], true);
    let id = result.session.unwrap().id;
    let saved = repository
        .open(&id)
        .unwrap()
        .metadata()
        .leaf_id
        .clone()
        .unwrap();
    let failed = host(
        root.path(),
        "store('counter',99); throw new Error('not committed');",
        false,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    failed
        .agent
        .run_with_session(
            &repository,
            SessionSelection::Existing(id.clone()),
            "fail",
            Arc::new(()),
        )
        .await
        .unwrap();
    let next = host(
        root.path(),
        "text(load('counter'));",
        false,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    let result = next
        .agent
        .run_with_session(
            &repository,
            SessionSelection::Existing(id.clone()),
            "load",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert_eq!(program_result(&result)["output"]["program"]["items"][0], 3);
    let mut session = repository.open(&id).unwrap();
    repository.checkout(&mut session, None).unwrap();
    let result = next
        .agent
        .run_with_session(
            &repository,
            SessionSelection::Existing(id.clone()),
            "empty branch",
            Arc::new(()),
        )
        .await
        .unwrap();
    assert_eq!(
        program_result(&result)["output"]["program"]["items"][0],
        Value::Null
    );
    let mut session = repository.open(&id).unwrap();
    repository.checkout(&mut session, Some(&saved)).unwrap();
}
#[tokio::test]
async fn vm_is_bounded_and_has_no_filesystem_network_node_timers_or_import_loader() {
    let root = tempfile::tempdir().unwrap();
    let mut config = CodeModeConfig::default();
    config.interrupt_checks = 10;
    let host = host(
        root.path(),
        "text([typeof process,typeof require,typeof fetch,typeof setTimeout]); while(true) {}",
        false,
        RunBudget::default(),
        config,
    );
    let result = tokio::time::timeout(Duration::from_secs(3), host.agent.run("run", Arc::new(())))
        .await
        .unwrap()
        .unwrap();
    let value = program_result(&result);
    assert_eq!(value["ok"], false);
    assert!(value["error"]["details"].to_string().contains("undefined"));
    assert_eq!(host.mode.active_workers(), 0);
}

#[tokio::test]
async fn cancellation_joins_the_vm_and_marks_unfinished_leaf_intents_without_late_results() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        "await tools.wait({});",
        false,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    let context = TurnExecutionContext::new();
    let worker = context.clone();
    let waiting = host.waiting.clone();
    let mode = host.mode.clone();
    let repository = Arc::new(MemorySessionRepository::new());
    let repo = repository.clone();
    let task = tokio::spawn(async move {
        host.agent
            .run_with_session_outcome(
                repo.as_ref(),
                SessionSelection::New,
                "run",
                Arc::new(()),
                worker,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), waiting.notified())
        .await
        .unwrap();
    context.cancel("cancel program");
    let outcome = task.await.unwrap();
    assert!(matches!(outcome, AgentTurnOutcome::Aborted(_)));
    assert_eq!(mode.active_workers(), 0);
    let session = repository.open(&outcome.session().unwrap().id).unwrap();
    let records = session
        .entries()
        .iter()
        .filter_map(|entry| match &entry.data {
            SessionEntryData::Extension { namespace, data }
                if namespace == COMPOSITION_NAMESPACE =>
            {
                serde_json::from_value::<CompositionRecord>(data.clone()).ok()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(records.iter().any(|record|matches!(record,CompositionRecord::Finished {status,abandoned,..} if status=="interrupted" && abandoned.len()==1)));
}
#[tokio::test]
async fn unawaited_promises_are_cancelled_and_do_not_outlive_success() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        "tools.wait({}); text('finished without awaiting');",
        false,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    let result = tokio::time::timeout(Duration::from_secs(3), host.agent.run("run", Arc::new(())))
        .await
        .unwrap()
        .unwrap();
    let value = program_result(&result);
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(value["output"]["composition"]["abandoned"], json!([1]));
    assert_eq!(host.mode.active_workers(), 0);
}
#[tokio::test]
async fn javascript_data_getters_can_reenter_output_but_exception_formatting_cannot_invoke_tools() {
    let root = tempfile::tempdir().unwrap();
    let code = "const input={toJSON(){text('nested serialization'); return {value:8};}}; text((await tools.echo(input)).value); console.log({toJSON(){text('console serialization'); return 'ok';}}); const error=new Error('original'); Object.defineProperty(error,'message',{get(){tools.mutate({value:99});return 'not data';}}); throw error;";
    let host = host(
        root.path(),
        code,
        false,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    let result = host.agent.run("run", Arc::new(())).await.unwrap();
    let value = program_result(&result);
    assert_eq!(value["ok"], false);
    assert!(
        value["error"]["details"]
            .to_string()
            .contains("nested serialization")
    );
    assert!(
        value["error"]["details"]
            .to_string()
            .contains("console serialization")
    );
    assert_eq!(host.mutations.load(Ordering::SeqCst), 0);
    assert_eq!(host.mode.active_workers(), 0);
}
#[tokio::test]
async fn output_memory_microtask_and_call_limits_fail_with_bounded_partial_data() {
    let root = tempfile::tempdir().unwrap();
    let mut output = CodeModeConfig::default();
    output.max_output_bytes = 512;
    let mut memory = CodeModeConfig::default();
    memory.memory_bytes = 1024 * 1024;
    let mut jobs = CodeModeConfig::default();
    jobs.max_jobs = 10;
    let mut calls = CodeModeConfig::default();
    calls.limits.max_calls = 2;
    for (code, config) in [
        ("text('partial'); return 'x'.repeat(2048);", output),
        (
            "text('partial'); const a=new Array(1000000).fill('long value'); return a.length;",
            memory,
        ),
        ("text('partial'); for(;;) await Promise.resolve();", jobs),
        (
            "text('partial'); await tools.echo({}); await tools.echo({}); await tools.echo({});",
            calls,
        ),
    ] {
        let host = host(root.path(), code, false, RunBudget::default(), config);
        let result =
            tokio::time::timeout(Duration::from_secs(3), host.agent.run("run", Arc::new(())))
                .await
                .unwrap()
                .unwrap();
        let value = program_result(&result);
        assert_eq!(value["ok"], false, "{value}");
        assert!(
            value["error"]["details"].to_string().contains("partial"),
            "{value}"
        );
        assert!(host.echo.load(Ordering::SeqCst) <= 2);
        assert_eq!(host.mode.active_workers(), 0);
    }
}
#[tokio::test]
async fn parent_duration_and_output_limits_stop_composition_without_a_fresh_budget() {
    let root = tempfile::tempdir().unwrap();
    let duration_host = host(
        root.path(),
        "await tools.wait({});",
        false,
        RunBudget {
            max_duration: Some(Duration::from_millis(50)),
            ..Default::default()
        },
        CodeModeConfig::default(),
    );
    let outcome = duration_host
        .agent
        .run_outcome("run", Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
    assert_eq!(duration_host.mode.active_workers(), 0);
    let host = host(
        root.path(),
        "text(await tools.echo({value:1}));",
        false,
        RunBudget {
            max_output_bytes: Some(1024),
            ..Default::default()
        },
        CodeModeConfig::default(),
    );
    let outcome = host
        .agent
        .run_outcome("run", Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
    assert_eq!(host.mode.active_workers(), 0);
}
#[derive(Clone, Copy)]
enum Fail {
    Call,
    Result,
    State,
    Unsupported,
}
struct FaultJournal(Fail);
#[async_trait]
impl RunJournal for FaultJournal {
    async fn checkpoint(&self, _: usize, _: &Message) -> anyhow::Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> anyhow::Result<()> {
        Ok(())
    }
    async fn record_composition(&self, record: &CompositionRecord) -> anyhow::Result<()> {
        if matches!(self.0, Fail::Unsupported)
            || matches!(
                (self.0, record),
                (Fail::Call, CompositionRecord::Call { .. })
                    | (Fail::Result, CompositionRecord::Result { .. })
            )
        {
            anyhow::bail!("scripted journal failure");
        }
        Ok(())
    }
    async fn commit_composition_state(&self, _: &str, _: &Value) -> anyhow::Result<()> {
        anyhow::bail!("scripted state failure")
    }
}
#[tokio::test]
async fn journal_failures_never_retry_completed_operations_or_admit_unrecorded_mutations() {
    let root = tempfile::tempdir().unwrap();
    for failure in [Fail::Call, Fail::Result, Fail::State, Fail::Unsupported] {
        let host = host(
            root.path(),
            "await tools.mutate({value:1}); store('done',true); text('done');",
            false,
            RunBudget::default(),
            CodeModeConfig::default(),
        );
        let request = ModelRequest::new("offline", vec![Message::text(MessageRole::User, "run")]);
        let outcome = host
            .agent
            .turn_executor()
            .run_with_journal(
                request,
                Arc::new(()),
                TurnExecutionContext::new(),
                &FaultJournal(failure),
            )
            .await;
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(
            host.mutations.load(Ordering::SeqCst),
            if matches!(failure, Fail::Call | Fail::Unsupported) {
                0
            } else {
                1
            }
        );
        assert_eq!(host.requests.lock().unwrap().len(), 1);
        assert_eq!(host.mode.active_workers(), 0);
    }
}
#[tokio::test]
async fn discovery_and_store_do_not_create_host_capabilities() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        "const matches=await searchTools('structured input',{limit:1}); text(matches[0].name); text((await describeTool('echo')).parameters.type); text(typeof tools.codemode); store('routing',{tool:'absent',permission:'allow_session'}); text(typeof tools.absent);",
        false,
        RunBudget::default(),
        CodeModeConfig::default(),
    );
    let result = host.agent.run("run", Arc::new(())).await.unwrap();
    let value = program_result(&result);
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(
        value["output"]["program"]["items"],
        json!(["echo", "object", "undefined", "undefined"])
    );
    assert_eq!(host.echo.load(Ordering::SeqCst), 0);
    assert_eq!(host.mutations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn composed_calls_cannot_exhaust_quota_then_let_a_sibling_root_mutation_escape() {
    let root = tempfile::tempdir().unwrap();
    let mutations = Arc::new(AtomicUsize::new(0));
    let echoes = Arc::new(AtomicUsize::new(0));
    let mode = Arc::new(CodeModeTool::new(CodeModeConfig::default()).unwrap());
    let mut registry = ToolRegistry::default();
    registry.register(Echo(echoes.clone())).unwrap();
    registry.register(Mutate(mutations.clone())).unwrap();
    registry.register_shared(mode.clone()).unwrap();
    let model = Model {
        once: false,
        code: "text((await tools.echo({value:1})).value);".into(),
        requests: Arc::new(Mutex::new(Vec::new())),
        trailing: vec![ModelToolCall {
            id: "later-root-mutation".into(),
            name: "mutate".into(),
            arguments: json!({"value":42}),
        }],
    };
    let agent = Agent::new(
        TurnExecutor::builder(model)
            .workspace(root.path())
            .registry(registry)
            .approval(AllowAllApproval)
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_tool_calls: Some(2),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap(),
        AgentConfig::new("offline"),
    );
    let outcome = agent
        .run_outcome("run", Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(matches!(outcome, AgentTurnOutcome::Limited(_)));
    assert_eq!(outcome.progress().tool_calls, 2);
    assert_eq!(echoes.load(Ordering::SeqCst), 1);
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
    assert_eq!(mode.active_workers(), 0);
}
#[tokio::test]
async fn vm_admission_is_bounded_and_dropped_program_cleanup_needs_no_tokio_task() {
    let mut config = CodeModeConfig::default();
    config.max_workers = 1;
    let tool = CodeModeTool::new(config).unwrap();
    let program = tool
        .start(
            CompositionContext {
                tools: Vec::new(),
                state: None,
            },
            json!({"code":"await new Promise(()=>{});"}),
        )
        .unwrap();
    assert!(
        tool.start(
            CompositionContext {
                tools: Vec::new(),
                state: None
            },
            json!({"code":"text('other');"})
        )
        .is_err()
    );
    drop(program);
    tokio::time::timeout(Duration::from_secs(3), async {
        while tool.active_workers() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn pending_approval(handle: &RunHandle) -> PendingRunApproval {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = handle.snapshot();
            if let Some(pending) = snapshot.approvals.first() {
                return pending.clone();
            }
            assert_ne!(snapshot.status, RunControlStatus::Finished);
            handle.changed(snapshot.revision).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn shared_run_controls_expire_leaf_approvals_on_revoke_steer_and_cancel() {
    for action in ["revoke", "steer", "cancel"] {
        let root = tempfile::tempdir().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let mode = Arc::new(CodeModeTool::new(CodeModeConfig::default()).unwrap());
        let mut registry = ToolRegistry::default();
        registry.register(Mutate(counter.clone())).unwrap();
        registry.register_shared(mode.clone()).unwrap();
        let agent = Agent::new(
            TurnExecutor::builder(Model {
                once: true,
                trailing: Vec::new(),
                requests: Arc::new(Mutex::new(Vec::new())),
                code: "await tools.mutate({value:1}); store('late',true);".into(),
            })
            .workspace(root.path())
            .registry(registry)
            .build()
            .unwrap(),
            AgentConfig::new("offline"),
        );
        let cache = ApprovalSessions::default();
        let scope = cache.session(root.path(), "composition").unwrap();
        let (handle, control) = RunHandle::channel(
            TurnExecutionContext::new().with_approval_session(scope.clone()),
            RunControlConfig {
                handle_approvals: true,
                ..Default::default()
            },
        )
        .unwrap();
        let run =
            tokio::spawn(async move { agent.run_controlled("run", Arc::new(()), control).await });
        let script = pending_approval(&handle).await;
        assert_eq!(script.invocation.name, "codemode");
        handle.respond_approval(&script.id, true).unwrap();
        let leaf = pending_approval(&handle).await;
        assert_eq!(leaf.invocation.name, "mutate");
        assert_eq!(
            leaf.invocation.preview.as_ref().unwrap().body,
            "{\"value\":1}"
        );
        match action {
            "revoke" => {
                scope.revoke_all();
                let _ = handle.respond_approval_decision(&leaf.id, ApprovalDecision::AllowSession);
            }
            "steer" => {
                handle
                    .steer(Message::text(MessageRole::User, "redirect"))
                    .unwrap();
            }
            _ => {
                handle.cancel("stop composition").unwrap();
            }
        }
        let outcome = tokio::time::timeout(Duration::from_secs(3), run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 0, "{action}");
        assert_eq!(mode.active_workers(), 0, "{action}");
        assert!(scope.snapshot().grants.is_empty());
        assert!(handle.snapshot().approvals.is_empty());
        assert!(
            handle
                .respond_approval_decision(&leaf.id, ApprovalDecision::AllowSession)
                .is_err()
        );
        if action == "cancel" {
            assert!(matches!(outcome, AgentTurnOutcome::Aborted(_)));
        } else {
            assert!(
                matches!(outcome, AgentTurnOutcome::Completed(_)),
                "{action}: {outcome:?}"
            );
        }
    }
}
struct PreparedComposite {
    mode: CodeModeTool,
    counter: Arc<AtomicUsize>,
}
#[async_trait]
impl ExecutableTool for PreparedComposite {
    fn definition(&self) -> ToolDefinition {
        self.mode.definition()
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Command
    }
    fn composition_factory(&self) -> Option<&dyn ToolCompositionFactory> {
        Some(&self.mode)
    }
    async fn prepare(
        &self,
        _: &ToolContext,
        _: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        Ok(Some(Box::new(Plan(
            self.counter.clone(),
            json!({"owned":true}),
        ))))
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        panic!("owned plan must not be replaced")
    }
}
#[tokio::test]
async fn composite_factory_cannot_silently_replace_an_owned_prepared_operation() {
    let root = tempfile::tempdir().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::default();
    registry
        .register(PreparedComposite {
            mode: CodeModeTool::new(CodeModeConfig::default()).unwrap(),
            counter: counter.clone(),
        })
        .unwrap();
    let approvals = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        TurnExecutor::builder(Model {
            once: true,
            trailing: Vec::new(),
            requests: Arc::new(Mutex::new(Vec::new())),
            code: "text('not run');".into(),
        })
        .workspace(root.path())
        .registry(registry)
        .approval(Approval {
            deny_mutation: false,
            requests: approvals.clone(),
        })
        .build()
        .unwrap(),
        AgentConfig::new("offline"),
    );
    let result = agent.run("run", Arc::new(())).await.unwrap();
    assert_eq!(
        program_result(&result)["error"]["code"],
        "composition_prepared_plan"
    );
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    assert!(approvals.lock().unwrap().is_empty());
}
#[tokio::test]
async fn syntax_exit_and_return_have_bounded_explicit_output_semantics() {
    for (code, expected, ok) in [
        ("return {value:42};", json!([{ "value":42 }]), true),
        (
            "text('before'); exit(); text('after');",
            json!(["before"]),
            true,
        ),
        ("await (", json!([]), false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let host = host(
            root.path(),
            code,
            false,
            RunBudget::default(),
            CodeModeConfig::default(),
        );
        let result = host.agent.run("run", Arc::new(())).await.unwrap();
        let body = program_result(&result);
        assert_eq!(body["ok"], ok, "{code}: {body}");
        let items = if ok {
            &body["output"]["program"]["items"]
        } else {
            &body["error"]["details"]["items"]
        };
        assert_eq!(*items, expected, "{code}: {body}");
        assert_eq!(host.mode.active_workers(), 0);
    }
}
struct PausingComposite {
    mode: CodeModeTool,
    cleaning: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}
struct PausingProgram {
    inner: Box<dyn ToolProgram>,
    cleaning: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl ToolProgram for PausingProgram {
    async fn next(&mut self) -> Result<ProgramEvent, ToolFailure> {
        self.inner.next().await
    }
    fn respond(&mut self, response: ProgramResponse) -> Result<(), ToolFailure> {
        self.inner.respond(response)
    }
    fn cancel(&mut self) {
        self.inner.cancel();
    }
    async fn shutdown(&mut self) -> Result<(), ToolFailure> {
        self.inner.shutdown().await?;
        self.cleaning.notify_one();
        self.resume.notified().await;
        Ok(())
    }
}
impl ToolCompositionFactory for PausingComposite {
    fn allowed_tools(&self) -> Option<&[String]> {
        self.mode.allowed_tools()
    }
    fn limits(&self) -> CompositionLimits {
        self.mode.limits()
    }
    fn state_namespace(&self) -> Option<&str> {
        self.mode.state_namespace()
    }
    fn start(
        &self,
        context: CompositionContext,
        args: Value,
    ) -> Result<Box<dyn ToolProgram>, ToolFailure> {
        Ok(Box::new(PausingProgram {
            inner: self.mode.start(context, args)?,
            cleaning: self.cleaning.clone(),
            resume: self.resume.clone(),
        }))
    }
}
#[async_trait]
impl ExecutableTool for PausingComposite {
    fn definition(&self) -> ToolDefinition {
        self.mode.definition()
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Command
    }
    fn composition_factory(&self) -> Option<&dyn ToolCompositionFactory> {
        Some(self)
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        unreachable!("scoped factory owns execution")
    }
}
#[tokio::test]
async fn steering_during_engine_cleanup_cannot_commit_already_finished_vm_state() {
    let root = tempfile::tempdir().unwrap();
    let repository = Arc::new(MemorySessionRepository::new());
    let cleaning = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let mode = Arc::new(PausingComposite {
        mode: CodeModeTool::new(CodeModeConfig::default()).unwrap(),
        cleaning: cleaning.clone(),
        resume: resume.clone(),
    });
    let mut registry = ToolRegistry::default();
    registry.register_shared(mode.clone()).unwrap();
    let agent = Agent::new(
        TurnExecutor::builder(Model {
            once: true,
            trailing: Vec::new(),
            requests: Arc::new(Mutex::new(Vec::new())),
            code: "store('late',true); text('produced before steering');".into(),
        })
        .workspace(root.path())
        .registry(registry)
        .approval(AllowAllApproval)
        .build()
        .unwrap(),
        AgentConfig::new("offline"),
    );
    let (handle, control) = RunHandle::pair();
    let repo = repository.clone();
    let worker = tokio::spawn(async move {
        agent
            .run_with_session_controlled(
                repo.as_ref(),
                SessionSelection::New,
                "run",
                Arc::new(()),
                control,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), cleaning.notified())
        .await
        .unwrap();
    assert_eq!(
        mode.mode.active_workers(),
        0,
        "VM must have really finished before steering"
    );
    handle
        .steer(Message::text(
            MessageRole::User,
            "redirect before state commit",
        ))
        .unwrap();
    resume.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .unwrap()
        .unwrap();
    let AgentTurnOutcome::Completed(result) = outcome else {
        panic!("redirected run must complete");
    };
    let body = program_result(&result);
    assert_eq!(body["error"]["code"], "composition_steered");
    assert!(
        body["error"]["details"]
            .to_string()
            .contains("produced before steering")
    );
    assert!(
        repository
            .extension_state(
                &result.session.unwrap().id,
                "aicoder.codemode.store.v1",
                65536
            )
            .unwrap()
            .is_none()
    );
}
