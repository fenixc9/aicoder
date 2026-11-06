//! Run `/ext review` in a real terminal. No model service, credentials or script runtime.
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
struct CustomWake;
#[async_trait]
impl ExtensionCommand for CustomWake {
    fn specification(&self) -> CommandSpec {
        CommandSpec {
            name: "custom".into(),
            description: "Start from sourced custom context".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn execute(
        &self,
        _: CommandRequest<'_>,
        _: ExtensionCancellation,
    ) -> anyhow::Result<CommandOutput> {
        Ok(CommandOutput::SubmitCustom(
            CustomMessage::text("unbound", "wake", "CUSTOM_IDLE_FACT")?
                .with_display(false)
                .with_details(json!({"inert":"CUSTOM_DETAILS_NOT_FOR_MODEL"}))?,
        ))
    }
}
struct RawUi;
#[async_trait]
impl InputHook for RawUi {
    async fn input(
        &self,
        input: InputHookInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<InputAction> {
        if input
            .input
            .message()
            .content
            .first()
            .is_some_and(|b| matches!(b,ContentBlock::Text{text}if text=="/raw-ui"))
        {
            anyhow::ensure!(
                input.source == InputSource::Interactive,
                "raw UI input must be interactive"
            );
            let answer = cancellation
                .ui()
                .ask(UiPrompt::Input {
                    title: "Raw input note".into(),
                    initial: String::new(),
                    multiline: true,
                    max_bytes: 256,
                })
                .await?;
            if let UiAnswer::Text { text } = answer {
                cancellation
                    .ui()
                    .notify(UiLevel::Info, format!("Input consumed: {text}"))?;
            }
            Ok(InputAction::Handled)
        } else {
            Ok(InputAction::Continue)
        }
    }
}
struct GeneralFinalize;
#[async_trait]
impl MessageHook for GeneralFinalize {
    async fn finalize(
        &self,
        input: MessageHookInput<'_>,
        _: ExtensionCancellation,
    ) -> anyhow::Result<Option<Message>> {
        Ok(
            match (input.message.role, input.message.text_content().as_str()) {
                (MessageRole::User, "message-finalization-demo") => {
                    Some(Message::text(MessageRole::User, "EFFECTIVE_TUI_USER"))
                }
                (MessageRole::Assistant, "Reviewed input: EFFECTIVE_TUI_USER") => Some(
                    Message::text(MessageRole::Assistant, "FINALIZED_ALL_ROLE_ANSWER"),
                ),
                _ => None,
            },
        )
    }
}
struct Offline;
struct Finalize;
#[async_trait]
impl AssistantMessageHook for Finalize {
    async fn finalize(
        &self,
        input: AssistantMessageInput<'_>,
        _: ExtensionCancellation,
    ) -> anyhow::Result<Option<Message>> {
        Ok(
            (input.message.text_content() == "Reviewed input: finalization-demo")
                .then(|| Message::text(MessageRole::Assistant, "FINALIZED_TUI_ANSWER")),
        )
    }
}
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
        if request
            .messages
            .last()
            .is_some_and(|m| m.text_content() == "model-wait")
        {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
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
    let storage = tempfile::tempdir()?;
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
            session_dir: Setting::Value(
                std::env::args_os()
                    .nth(1)
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| storage.path().into()),
            ),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::<String, String>::new()),
    )?;
    let mut extension = Extension::new("review-example", "1")?;
    extension.command(Arc::new(Review), Duration::from_secs(300))?;
    extension.command(Arc::new(CustomWake), Duration::from_secs(10))?;
    let mut raw = HookSpec::new("raw-input");
    raw.timeout = Duration::from_secs(10);
    extension.input_hook(raw, Arc::new(RawUi))?;
    extension.assistant_message_hook(HookSpec::new("final-answer"), Arc::new(Finalize))?;
    extension.message_hook(
        HookSpec::new("all-role-final-answer"),
        Arc::new(GeneralFinalize),
    )?;
    let mut registry = ExtensionRegistry::new();
    registry.install(extension)?;
    aicoder_tui::run_with_provider(runtime, Arc::new(Offline), Arc::new(registry.finish())).await
}
