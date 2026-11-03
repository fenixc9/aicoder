//! User input boundary shared by text and multimodal Agent entry points.
use crate::model::{ContentBlock, ImageSource, Message, MessageRole};
use anyhow::{Result, ensure};
use std::io::Write;
/// A user turn containing text and/or images. String conversions preserve text-only callers.
/// Explicit messages reject non-user roles, non-input blocks and oversized image envelopes.
/// Image URLs belong to the host; core never fetches or decodes image bytes.
#[derive(Debug, Clone)]
pub struct AgentInput(Message);
impl AgentInput {
    pub fn message(&self) -> &Message {
        &self.0
    }
    pub fn into_message(self) -> Message {
        self.0
    }
}
impl From<String> for AgentInput {
    fn from(value: String) -> Self {
        Self(Message::text(MessageRole::User, value))
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
                            media_type.starts_with("image/")
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
        Ok(Self(message))
    }
}

fn inline_base64(data: &str) -> bool {
    if data.is_empty() || !data.len().is_multiple_of(4) {
        return false;
    }
    let body = data.trim_end_matches('=');
    data.len() - body.len() <= 2
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}
