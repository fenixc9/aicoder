//! Optional real JS composition with actual file tools and canonical JSONL records.
use aicoder_core::{model::*, session::*, tools::*, *};
use aicoder_runtime::{code_mode::CodeModeSettings, resources::ResourceSettings, *};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
struct Model(Arc<Mutex<Vec<ModelRequest>>>);
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
        self.0.lock().unwrap().push(request.clone());
        let call = request.messages.last().unwrap().role == MessageRole::User;
        let code = "const found = await searchTools('read file', {limit:2}); const reads = await Promise.allSettled([tools.read_file({path:'a.txt'}),tools.read_file({path:'b.txt'})]); text(reads.map(r=>r.value.content.trim())); store('count',(load('count')||0)+1); text(load('count'));";
        let message = if call {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "program".into(),
                        name: "codemode".into(),
                        arguments: json!({"code":code}),
                    },
                }],
            }
        } else {
            Message::text(
                MessageRole::Assistant,
                "Filtered real file results received",
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
struct Approve(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl ApprovalHandler for Approve {
    async fn approve(&self, invocation: &ToolInvocation) -> anyhow::Result<bool> {
        self.0.lock().unwrap().push(invocation.name.clone());
        Ok(true)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let storage = tempfile::tempdir()?;
    std::fs::write(workspace.path().join("a.txt"), "alpha\n")?;
    std::fs::write(workspace.path().join("b.txt"), "beta\n")?;
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                model: Setting::Value("offline".into()),
                code_mode: CodeModeSettings {
                    enabled: Setting::Value(true),
                    tools: Setting::Value(vec!["read_file".into()]),
                },
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Value(storage.path().into()),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::<String, String>::new()),
    )?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let approvals = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        runtime
            .executor_builder(Arc::new(Model(requests.clone())))?
            .approval(Approve(approvals.clone()))
            .build()?,
        runtime.agent_config(),
    );
    let repository = runtime.sessions()?;
    let first = agent
        .run_with_session(
            &repository,
            SessionSelection::New,
            "compose actual reads",
            Arc::new(()),
        )
        .await?;
    let id = first.session.unwrap().id;
    drop(repository);
    let repository = runtime.sessions()?; // Cold JSONL reopen, not an in-memory state cache.
    let second = agent
        .run_with_session(
            &repository,
            SessionSelection::Existing(id.clone()),
            "resume branch-local store",
            Arc::new(()),
        )
        .await?;
    let session = repository.open(&id)?;
    let records=session.entries().iter().filter(|entry|matches!(&entry.data,SessionEntryData::Extension {namespace,..} if namespace==aicoder_core::composition::COMPOSITION_NAMESPACE)).count();
    anyhow::ensure!(records >= 12, "nested canonical records missing");
    anyhow::ensure!(
        first.execution_result.tool_calls == 3 && second.execution_result.tool_calls == 3,
        "parent budget omitted actual calls"
    );
    let items = second
        .execution_result
        .messages
        .iter()
        .filter_map(Message::tool_result)
        .next_back()
        .unwrap()
        .1["output"]["program"]["items"]
        .clone();
    anyhow::ensure!(
        items[0] == json!(["alpha", "beta"]) && items[1] == 2,
        "JS filtering/store persistence failed: {items}"
    );
    anyhow::ensure!(
        *approvals.lock().unwrap() == ["codemode", "codemode"],
        "unexpected leaf permission path"
    );
    println!(
        "{}",
        json!({"items":items,"root_tool_calls":second.execution_result.tool_calls,"usage":second.execution_result.usage,"canonical_composition_records":records,"model_requests":requests.lock().unwrap().len(),"script_approvals":approvals.lock().unwrap().len(),"network_requests":0})
    );
    Ok(())
}
