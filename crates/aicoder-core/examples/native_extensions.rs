//! Native extension assembly and plan-mode interception without network, credentials or a new loop.
use aicoder_core::{extensions::*, model::*, tools::*, *};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Planning(Arc<AtomicBool>);
#[async_trait]
impl ContextHook for Planning {
    async fn context(
        &self,
        _: ContextHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<String>> {
        Ok(self
            .0
            .load(Ordering::SeqCst)
            .then(|| "Planning mode: inspect and propose changes without modifying files.".into()))
    }
}
#[async_trait]
impl ToolHook for Planning {
    async fn before_tool(
        &self,
        input: ToolHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<ToolHookDecision> {
        Ok(
            if self.0.load(Ordering::SeqCst) && input.capability != ToolCapability::ReadOnly {
                ToolHookDecision::Deny {
                    reason: "Planning blocks mutation and shell commands".into(),
                }
            } else {
                ToolHookDecision::Continue
            },
        )
    }
}
#[async_trait]
impl ExtensionCommand for Planning {
    fn specification(&self) -> CommandSpec {
        CommandSpec {
            name: "plan".into(),
            description: "Enter or leave planning mode".into(),
            parameters: json!({"type":"object","properties":{"enabled":{"type":"boolean"}},"required":["enabled"]}),
        }
    }
    async fn execute(
        &self,
        request: CommandRequest<'_>,
        _: ExtensionCancellation,
    ) -> Result<CommandOutput> {
        let enabled = request.arguments["enabled"]
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("enabled must be boolean"))?;
        self.0.store(enabled, Ordering::SeqCst);
        Ok(if enabled {
            CommandOutput::Submit("Inspect the request and prepare a plan".into())
        } else {
            CommandOutput::Notice("Planning disabled; normal tool approval still applies".into())
        })
    }
}
struct Note(Arc<AtomicUsize>);
#[async_trait]
impl ExecutableTool for Note {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_note".into(),
            description: Some("An example mutating tool".into()),
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess::new(json!({"saved":true})))
    }
}
struct Provider(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl ModelProvider for Provider {
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
        let last = request.messages.last().unwrap();
        let mut response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "Plan ready; no mutation performed"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        if last.role == MessageRole::User {
            response.stop_reason = StopReason::ToolCalls;
            response.message.content = vec![ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: "note".into(),
                    name: "save_note".into(),
                    arguments: json!({}),
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
struct Observe(Arc<AtomicUsize>);
impl AgentEventHandler for Observe {
    fn on_raw_event(&self, _: &AgentRawEventEnvelope) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let planning = Arc::new(AtomicBool::new(false));
    let mutations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(vec![]));
    let mut extension = Extension::new("example.planning", "1.0.0")?;
    extension
        .provider("offline", Arc::new(Provider(results.clone())))?
        .tool(Arc::new(Note(mutations.clone())))?
        .context_policy("window", Arc::new(context_policy::WindowContextPolicy))?
        .context_hook(
            HookSpec::new("instructions"),
            Arc::new(Planning(planning.clone())),
        )?
        .tool_hook(
            HookSpec::new("read-only"),
            Arc::new(Planning(planning.clone())),
        )?
        .command(Arc::new(Planning(planning)), Duration::from_secs(1))?
        .observer("trace", 0, Arc::new(Observe(observed.clone())))?;
    let mut registry = ExtensionRegistry::new();
    registry.reserve_command("exit")?;
    registry.install(extension)?;
    let extensions = Arc::new(registry.finish());
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(extensions.provider("offline").unwrap())
            .workspace(workspace.path())
            .extensions(extensions.clone())
            .context_policy(extensions.context_policy("window").unwrap())
            .build()?,
        AgentConfig::new("offline-model"),
    );
    for enabled in [true, false] {
        let arguments = json!({"enabled":enabled});
        let command = extensions
            .execute_command(
                "plan",
                CommandRequest {
                    context: CommandContext {
                        workspace: workspace.path(),
                        session: None,
                    },
                    arguments: &arguments,
                },
                &TurnExecutionContext::new(),
            )
            .await?;
        let input = match command {
            CommandOutput::Submit(input) => input,
            CommandOutput::Notice(_) => "Attempt the example mutation".into(),
        };
        agent.run(input, Arc::new(())).await?;
    }
    ensure!(
        mutations.load(Ordering::SeqCst) == 0,
        "extension bypassed mutation approval"
    );
    ensure!(
        *results.lock().unwrap() == vec!["extension_denied", "approval_denied"],
        "unexpected tool/approval path"
    );
    println!(
        "{}",
        json!({"catalog":extensions.catalog(),"commands":extensions.commands(),"tool_results":*results.lock().unwrap(),"observed_events":observed.load(Ordering::SeqCst),"mutations":0,"network_requests":0})
    );
    Ok(())
}
