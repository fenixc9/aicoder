//! User input boundary shared by text and multimodal Agent entry points.
use crate::model::{ContentBlock, ImageSource, Message, MessageRole};
use anyhow::{Result, ensure};
use std::io::Write;
/// A user turn containing text and/or images. String conversions preserve text-only callers.
/// Explicit messages reject non-user roles, non-input blocks and oversized image envelopes.
/// Image URLs belong to the host; core never fetches or decodes image bytes.
#[derive(Debug, Clone)]
pub struct AgentInput {
    message: Message,
    source: InputSource,
    pub(crate) prepared: Option<crate::extensions::PreparedUserInput>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    #[default]
    Sdk,
    Interactive,
    Rpc,
    Web,
    Extension,
}

impl AgentInput {
    pub(crate) fn replace_finalized_message(&mut self, message: Message) -> Result<()> {
        let validated = Self::try_from(message)?;
        self.message = validated.message;
        Ok(())
    }
    pub fn preparation_receipt(&self) -> Option<&crate::extensions::InputReceipt> {
        self.prepared.as_ref().and_then(|p| p.receipt.as_deref())
    }
    pub fn source(&self) -> InputSource {
        self.source
    }
    pub fn with_source(mut self, source: InputSource) -> Self {
        self.source = source;
        self.prepared = None;
        self
    }
    /// Replace the prompt block, preserving separately captured text/image attachments.
    pub fn with_prompt_text(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        if let Some(ContentBlock::Text { text: original }) = self.message.content.first_mut() {
            *original = text;
        } else {
            self.message.content.insert(0, ContentBlock::Text { text });
        }
        self.prepared = None;
        self
    }

    pub fn message(&self) -> &Message {
        &self.message
    }
    pub fn into_message(self) -> Message {
        self.message
    }
}
impl From<String> for AgentInput {
    fn from(value: String) -> Self {
        Self {
            message: Message::text(MessageRole::User, value),
            source: InputSource::Sdk,
            prepared: None,
        }
    }
}
impl From<&str> for AgentInput {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}
impl From<&String> for AgentInput {
    fn from(value: &String) -> Self {
        value.as_str().into()
    }
}
impl TryFrom<Message> for AgentInput {
    type Error = anyhow::Error;
    fn try_from(message: Message) -> Result<Self> {
        ensure!(
            message.role == MessageRole::User && !message.content.is_empty(),
            "agent input must be a nonempty user message"
        );
        let mut images = 0;
        for block in &message.content {
            match block {
                ContentBlock::Text { .. } => {}
                ContentBlock::Image { source } => {
                    images += 1;
                    match source {
                        ImageSource::Artifact { .. } => {
                            anyhow::bail!("tool artifact images are not user input")
                        }
                        ImageSource::Url { url } => {
                            let parsed = url::Url::parse(url)?;
                            ensure!(
                                matches!(parsed.scheme(), "http" | "https")
                                    && parsed.host_str().is_some()
                                    && parsed.username().is_empty()
                                    && parsed.password().is_none(),
                                "image URL must be HTTP(S) without credentials"
                            );
                        }
                        ImageSource::Base64 { media_type, data } => ensure!(
                            crate::media::image_media_type(media_type).is_some()
                                && media_type.len() <= 128
                                && media_type.is_ascii()
                                && !media_type.chars().any(char::is_control)
                                && inline_base64(data),
                            "invalid inline image source"
                        ),
                    }
                }
                _ => anyhow::bail!("agent input accepts user text/images only"),
            }
        }
        ensure!(images <= 4, "agent input exceeds four images");
        if images > 0 {
            struct Limit(usize);
            impl Write for Limit {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    if bytes.len() > (2 * 1024 * 1024 - 16384) - self.0 {
                        return Err(std::io::Error::other(
                            "image-bearing user message exceeds 2 MiB minus metadata headroom",
                        ));
                    }
                    self.0 += bytes.len();
                    Ok(bytes.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            serde_json::to_writer(&mut Limit(0), &message)?;
        }
        Ok(Self {
            message,
            source: InputSource::Sdk,
            prepared: None,
        })
    }
}

pub(crate) fn inline_base64(data: &str) -> bool {
    if data.is_empty() || !data.len().is_multiple_of(4) {
        return false;
    }
    let body = data.trim_end_matches('=');
    data.len() - body.len() <= 2
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}
