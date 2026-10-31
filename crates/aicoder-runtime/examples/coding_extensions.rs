//! Planning, protected prepared targets and managed read-only workers over the existing core.
#[path = "support/coding_safety.rs"]
mod coding_safety;
#[path = "support/read_only_workers.rs"]
mod read_only_workers;
use aicoder_core::{extensions::*, model::*, session::*, tools::*, *};
use aicoder_runtime::{
    session::JsonlSessionRepository,
    tools::{ReadFileTool, WriteFileTool},
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use coding_safety::CodingSafety;
use read_only_workers::{ReadOnlyWorkers, WorkerBudget, WorkerProfile};
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct ParentModel(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl ModelProvider for ParentModel {
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
        let last = request.messages.last().unwrap();
        let mut response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "Operation reported"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        if last.role == MessageRole::User {
            let text = last.text_content();
            let path = if text.contains("alias") {
                "alias.txt"
            } else if text.contains("protected") {
                "protected/config"
            } else {
                "note.txt"
            };
            response.stop_reason = StopReason::ToolCalls;
            response.message.content = vec![ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "write".into(),
                    name: "write_file".into(),
                    arguments: json!({"path":path,"content":format!("change: {text}")}),
                },
            }];
        } else if let Some((_, result, _)) = last.tool_result() {
            self.0
                .lock()
                .unwrap()
                .push(result["error"]["code"].as_str().unwrap_or("success").into());
        }
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct ReviewModel;
#[async_trait]
impl ModelProvider for ReviewModel {
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
        let call = request.messages.last().unwrap().role == MessageRole::User;
        let message = if call {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "read".into(),
                        name: "read_file".into(),
                        arguments: json!({"path":"src/lib.rs"}),
                    },
                }],
            }
        } else {
            let data = request
                .messages
                .last()
                .unwrap()
                .tool_result()
                .map(|(_, value, _)| value.to_string())
                .unwrap_or_default();
            Message::text(
                MessageRole::Assistant,
                format!("Read-only review received: {data}"),
            )
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
            usage: ModelUsage {
                input_tokens: Some(12),
                output_tokens: Some(3),
                total_tokens: Some(15),
                ..Default::default()
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Approval(Arc<AtomicUsize>);
#[async_trait]
impl ApprovalHandler for Approval {
    async fn decide(&self, _: &ApprovalRequest<'_>) -> Result<ApprovalDecision> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ApprovalDecision::AllowSession)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let storage = tempfile::tempdir()?;
    std::fs::create_dir(workspace.path().join("protected"))?;
    std::fs::create_dir(workspace.path().join("src"))?;
    std::fs::write(workspace.path().join("protected/config"), "KEEP")?;
    std::fs::write(workspace.path().join("note.txt"), "before")?;
    std::fs::write(workspace.path().join("src/lib.rs"), "pub fn reviewed() {}")?;
    std::os::unix::fs::symlink("protected/config", workspace.path().join("alias.txt"))?;
    let safety = CodingSafety::new(workspace.path(), &["protected"])?;
    let mut worker_tools = ToolRegistry::default();
    worker_tools.register(ReadFileTool)?;
    let child_repository = Arc::new(JsonlSessionRepository::new(storage.path())?);
    let workers = ReadOnlyWorkers::new(
        workspace.path(),
        vec![WorkerProfile::new(
            "review",
            workspace.path(),
            Arc::new(ReviewModel),
            "offline-review",
            worker_tools,
            WorkerBudget::default(),
        )?],
        child_repository.clone(),
        2,
    )?;
    let mut extension = Extension::new("coding-examples", "1")?;
    extension
        .context_hook(HookSpec::new("planning-context"), safety.clone())?
        .tool_hook(HookSpec::new("planning-guard"), safety.clone())?
        .command(safety.clone(), Duration::from_secs(5))?
        .command(Arc::new(workers.clone()), Duration::from_secs(5))?
        .tool(Arc::new(safety.wrap(Arc::new(WriteFileTool))))?;
    let mut registry = ExtensionRegistry::new();
    registry.install(extension)?;
    let extensions = Arc::new(registry.finish());
    let results = Arc::new(Mutex::new(Vec::new()));
    let approvals = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        TurnExecutor::builder(ParentModel(results.clone()))
            .workspace(workspace.path())
            .extensions(extensions.clone())
            .approval(Approval(approvals.clone()))
            .build()?,
        AgentConfig::new("offline-parent"),
    );
    let parent_repository = MemorySessionRepository::new();
    let parent = parent_repository.create(workspace.path())?;
    let id = parent.metadata().id.clone();
    let grants = ApprovalSessions::default();
    for (enabled, prompt) in [
        (true, "planned write"),
        (false, "allowed write"),
        (true, "blocked despite cached grant"),
        (false, "protected write"),
        (false, "alias write"),
    ] {
        extensions
            .execute_command(
                "plan",
                CommandRequest {
                    context: CommandContext {
                        workspace: workspace.path(),
                        session: None,
                    },
                    arguments: &json!({"enabled":enabled}),
                },
                &TurnExecutionContext::new(),
            )
            .await?;
        agent
            .run_with_session_context(
                &parent_repository,
                SessionSelection::Existing(id.clone()),
                prompt,
                Arc::new(()),
                TurnExecutionContext::new().with_approval_sessions(grants.clone()),
            )
            .await?;
    }
    ensure!(!safety.planning().await, "planning state incorrect");
    ensure!(
        approvals.load(Ordering::SeqCst) == 1,
        "cached approval or plan guard was bypassed"
    );
    ensure!(
        *results.lock().unwrap()
            == vec![
                "extension_denied",
                "success",
                "extension_denied",
                "protected_path",
                "protected_path"
            ],
        "unexpected policy results"
    );
    ensure!(
        std::fs::read_to_string(workspace.path().join("protected/config"))? == "KEEP",
        "protected file changed"
    );
    let parent = parent_repository.open(&id)?;
    let before = parent.entries().len();
    let output=extensions.execute_command("worker",CommandRequest {context:CommandContext {workspace:workspace.path(),session:Some(parent.metadata())},arguments:&json!({"action":"start","profile":"review","task":"Review src/lib.rs without changes"})},&TurnExecutionContext::new()).await?;
    let CommandOutput::Notice(output) = output else {
        anyhow::bail!("worker start must not submit parent input");
    };
    let job = serde_json::from_str::<serde_json::Value>(&output)?["started"]
        .as_str()
        .unwrap()
        .to_string();
    let report = workers.wait(&job).await?;
    ensure!(
        report.status == "completed"
            && report.trace_complete
            && report.usage.as_ref().and_then(|u| u.total_tokens) == Some(30),
        "worker core outcome/trace/usage incomplete"
    );
    let page = workers.trace_page(&job, 0, 50)?;
    ensure!(
        !page["frames"].as_array().unwrap().is_empty(),
        "worker trace missing"
    );
    let child = child_repository.open(report.session_id.as_deref().unwrap())?;
    ensure!(child.entries().iter().any(|entry|matches!(&entry.data,SessionEntryData::Extension {namespace,..} if namespace=="example.worker_origin.v1")),"child lineage missing");
    ensure!(
        parent_repository.open(&id)?.entries().len() == before,
        "worker polluted parent history"
    );
    let _status = workers.status(&job)?;
    workers.shutdown().await;
    workers.forget(&job)?;
    ensure!(
        workers.start("review", "late".into(), None).is_err(),
        "shutdown reopened admission"
    );
    println!(
        "{}",
        json!({"policy_results":*results.lock().unwrap(),"approvals":approvals.load(Ordering::SeqCst),"protected_unchanged":true,"worker":report,"trace_frames_in_page":page["frames"].as_array().unwrap().len(),"parent_history_unchanged":true,"network_requests":0})
    );
    Ok(())
}
