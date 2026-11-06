//! Explicit run triggers. Custom messages are canonical extension facts, never user submissions.
use crate::{AgentInput, TurnExecutionContext, TurnExecutor, extensions::*, model::*};
use anyhow::{Result, ensure};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;
use std::sync::Arc;
pub const CUSTOM_MESSAGE_NAMESPACE: &str = "aicoder.custom-message.v1";
pub const MAX_CUSTOM_MESSAGE_BYTES: usize = 2 * 1024 * 1024 - 4096;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomData {
    source: String,
    custom_type: String,
    display: bool,
    details: Option<Value>,
    message: Message,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CustomMessage(Arc<CustomData>);
impl<'de> Deserialize<'de> for CustomMessage {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let value = Self(Arc::new(CustomData::deserialize(d)?));
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomMessageMetadata {
    pub source: String,
    pub custom_type: String,
    pub display: bool,
    pub details: Option<Value>,
}
impl CustomMessage {
    pub fn metadata(&self) -> CustomMessageMetadata {
        CustomMessageMetadata {
            source: self.source().into(),
            custom_type: self.custom_type().into(),
            display: self.display(),
            details: self.details().cloned(),
        }
    }
    pub(crate) fn from_canonical(
        metadata: CustomMessageMetadata,
        message: Message,
    ) -> Result<Self> {
        let value = Self(Arc::new(CustomData {
            source: metadata.source,
            custom_type: metadata.custom_type,
            display: metadata.display,
            details: metadata.details,
            message,
        }));
        value.validate()?;
        Ok(value)
    }
    pub fn new(
        source: impl Into<String>,
        custom_type: impl Into<String>,
        content: Vec<ContentBlock>,
    ) -> Result<Self> {
        let source = source.into();
        let custom_type = custom_type.into();
        let mut blocks = vec![ContentBlock::Text {
            text: label(&source, &custom_type),
        }];
        blocks.extend(content);
        let value = Self(Arc::new(CustomData {
            source,
            custom_type,
            display: true,
            details: None,
            message: Message {
                role: MessageRole::Custom,
                content: blocks,
            },
        }));
        value.validate()?;
        Ok(value)
    }
    pub fn text(
        source: impl Into<String>,
        custom_type: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Self> {
        Self::new(
            source,
            custom_type,
            vec![ContentBlock::Text { text: text.into() }],
        )
    }
    pub fn source(&self) -> &str {
        &self.0.source
    }
    pub fn custom_type(&self) -> &str {
        &self.0.custom_type
    }
    pub fn display(&self) -> bool {
        self.0.display
    }
    pub fn details(&self) -> Option<&Value> {
        self.0.details.as_ref()
    }
    pub fn message(&self) -> &Message {
        &self.0.message
    }
    /// Compatibility projection only; keep message() / typed entries as recorded history.
    pub fn model_message(&self) -> Message {
        let mut message = self.message().clone();
        message.role = MessageRole::User;
        message
    }
    pub fn content(&self) -> &[ContentBlock] {
        &self.0.message.content[1..]
    }
    pub fn with_display(mut self, display: bool) -> Self {
        Arc::make_mut(&mut self.0).display = display;
        self
    }
    pub fn with_details(mut self, details: Value) -> Result<Self> {
        Arc::make_mut(&mut self.0).details = Some(details);
        self.validate()?;
        Ok(self)
    }
    pub(crate) fn with_source(mut self, source: &str) -> Result<Self> {
        let value = Arc::make_mut(&mut self.0);
        value.source = source.into();
        value.message.content[0] = ContentBlock::Text {
            text: label(source, &value.custom_type),
        };
        self.validate()?;
        Ok(self)
    }
    pub fn validate(&self) -> Result<()> {
        crate::extensions::name(self.source())?;
        crate::extensions::name(self.custom_type())?;
        validate_custom_message(self.message())?;
        ensure!(
            self.message().content.first()
                == Some(&ContentBlock::Text {
                    text: label(self.source(), self.custom_type())
                }),
            "custom message source differs from its canonical label"
        );
        if let Some(details) = self.details() {
            crate::extensions::size(details, 32 * 1024)?;
        }
        crate::extensions::size(self, MAX_CUSTOM_MESSAGE_BYTES)?;
        Ok(())
    }
}
fn label(source: &str, kind: &str) -> String {
    format!("[aicoder custom {source}/{kind}]\n")
}
pub(crate) fn validate_custom_message(message: &Message) -> Result<()> {
    ensure!(
        message.role == MessageRole::Custom,
        "custom message has wrong role"
    );
    let Some(ContentBlock::Text { text }) = message.content.first() else {
        anyhow::bail!("custom message lacks source label")
    };
    let Some(value) = text
        .strip_prefix("[aicoder custom ")
        .and_then(|v| v.strip_suffix("]\n"))
    else {
        anyhow::bail!("invalid custom message label")
    };
    let Some((source, kind)) = value.split_once('/') else {
        anyhow::bail!("invalid custom message source")
    };
    crate::extensions::name(source)?;
    crate::extensions::name(kind)?;
    if message.content.len() > 1 {
        AgentInput::try_from(Message {
            role: MessageRole::User,
            content: message.content[1..].to_vec(),
        })?;
    }
    crate::extensions::size(message, MAX_CUSTOM_MESSAGE_BYTES)?;
    Ok(())
}
#[derive(Debug, Clone)]
pub enum AgentPrompt {
    User(AgentInput),
    Custom(CustomMessage),
}
impl From<AgentInput> for AgentPrompt {
    fn from(input: AgentInput) -> Self {
        Self::User(input)
    }
}
impl From<CustomMessage> for AgentPrompt {
    fn from(input: CustomMessage) -> Self {
        Self::Custom(input)
    }
}
impl From<String> for AgentPrompt {
    fn from(input: String) -> Self {
        Self::User(input.into())
    }
}
impl From<&str> for AgentPrompt {
    fn from(input: &str) -> Self {
        Self::User(input.into())
    }
}
impl From<&String> for AgentPrompt {
    fn from(input: &String) -> Self {
        Self::User(input.into())
    }
}
#[derive(Debug, Clone)]
pub enum RunTrigger {
    User,
    Custom(CustomMessage),
}
pub(crate) enum PromptPreparation {
    Ready(AgentPrompt),
    Handled(InputHandled),
}
impl AgentPrompt {
    pub fn message(&self) -> &Message {
        match self {
            Self::User(input) => input.message(),
            Self::Custom(input) => input.message(),
        }
    }
    pub(crate) fn prepared_user(&self) -> Option<&PreparedUserInput> {
        match self {
            Self::User(input) => input.prepared.as_ref(),
            Self::Custom(_) => None,
        }
    }
    pub(crate) fn trigger(&self) -> RunTrigger {
        match self {
            Self::User(_) => RunTrigger::User,
            Self::Custom(c) => RunTrigger::Custom(c.clone()),
        }
    }
    pub(crate) fn into_message(self) -> Message {
        match self {
            Self::User(input) => input.into_message(),
            Self::Custom(custom) => custom.message().clone(),
        }
    }
    pub(crate) fn entry(self) -> crate::session::SessionEntryData {
        match self {
            Self::User(input) => crate::session::SessionEntryData::Message {
                message: input.into_message(),
            },
            Self::Custom(custom) => crate::session::SessionEntryData::CustomMessage { custom },
        }
    }
    pub(crate) async fn prepare(
        self,
        executor: &TurnExecutor,
        context: &TurnExecutionContext,
        started: tokio::time::Instant,
    ) -> Result<PromptPreparation> {
        if let Some(control) = &context.control {
            control.check_input_pipeline()?;
        }
        match self {
            Self::User(input) => Ok(
                match executor
                    .prepare_user_input_at(input, InputDelivery::Initial, context, started)
                    .await?
                {
                    InputPreparation::Ready(input) => PromptPreparation::Ready(Self::User(input)),
                    InputPreparation::Handled(handled) => PromptPreparation::Handled(handled),
                },
            ),
            Self::Custom(custom) => {
                custom.validate()?;
                if context.is_cancelled() {
                    return Err(context.error().into());
                }
                Ok(PromptPreparation::Ready(Self::Custom(custom)))
            }
        }
    }
}
