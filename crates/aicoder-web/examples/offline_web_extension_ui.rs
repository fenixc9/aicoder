//! Temporary offline Web UI with a native review command. Prints its ephemeral connection info.
use aicoder_core::{extensions::*, model::*, ui::*, *};
use aicoder_runtime::{
    ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings, resources::ResourceSettings,
};
use async_trait::async_trait;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
struct Review;
#[async_trait]
impl ExtensionCommand for Review {
    fn specification(&self) -> CommandSpec {
        CommandSpec {
            name: "review".into(),
            description: "Select a review action and enter a note".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn execute(
        &self,
        _: CommandRequest<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<CommandOutput> {
        let ui = cancellation.ui();
        if !ui.capabilities().select {
            return Ok(CommandOutput::Notice(
                "Review needs an interactive host".into(),
            ));
        }
        ui.notify(UiLevel::Info, "Review command ready".into())?;
        ui.status("review", Some("Choosing review action".into()))?;
        let choice = ui
            .ask(UiPrompt::Select {
                title: "Review action".into(),
                options: vec![
                    UiOption {
                        id: "notice".into(),
                        label: "Keep a note".into(),
                    },
                    UiOption {
                        id: "submit".into(),
                        label: "Ask the agent".into(),
                    },
                ],
            })
            .await?;
        if choice == UiAnswer::Dismissed {
            return Ok(CommandOutput::Notice("Review dismissed".into()));
        }
        ui.status("review", Some("Entering review note".into()))?;
        let UiAnswer::Text { text } = ui
            .ask(UiPrompt::Input {
                title: "Review note".into(),
                initial: String::new(),
                multiline: true,
                max_bytes: 1024,
            })
            .await?
        else {
            return Ok(CommandOutput::Notice("Review dismissed".into()));
        };
        ui.status("review", None)?;
        match choice {
            UiAnswer::Choice { id } if id == "submit" => Ok(CommandOutput::Submit(
                format!("Review request: {text}").into(),
            )),
            _ => Ok(CommandOutput::Notice(format!("Review noted: {text}"))),
        }
    }
}
struct Offline;
#[async_trait]
impl ModelProvider for Offline {
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
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(
                MessageRole::Assistant,
                format!(
                    "Reviewed input: {}",
                    request
                        .messages
                        .last()
                        .map(Message::text_content)
                        .unwrap_or_default()
                ),
            ),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                model: Setting::Value("offline".into()),
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Clear,
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::<String, String>::new()),
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let token = std::env::var("AICODER_WEB_TOKEN").map_err(|_| {
        anyhow::anyhow!("Set an ephemeral AICODER_WEB_TOKEN for this temporary example")
    })?;
    let mut extension = Extension::new("review-example", "1")?;
    extension.command(Arc::new(Review), Duration::from_secs(300))?;
    let mut registry = ExtensionRegistry::new();
    registry.install(extension)?;
    let options = aicoder_web::WebOptions::new(token.clone(), address)?
        .memory_sessions()
        .extensions(Arc::new(registry.finish()));
    let host = aicoder_web::WebHost::with_provider(runtime, options, Arc::new(Offline))?;
    let router = host.router();
    let (close, closed) = tokio::sync::oneshot::channel::<()>();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = closed.await;
            })
            .await
    });
    use std::io::Write;
    println!(
        "{}",
        json!({"url":format!("http://{address}"),"temporary_workspace":true,"offline":true})
    );
    std::io::stdout().flush()?;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let ended = tokio::select! {result=&mut server=>Some(result),_=tokio::signal::ctrl_c()=>None,_=term.recv()=>None};
    host.shutdown().await;
    let _ = close.send(());
    if let Some(result) = ended {
        result??;
    } else {
        server.await??;
    }
    Ok(())
}
