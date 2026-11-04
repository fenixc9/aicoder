//! Explicit context messages have a typed domain form and a backward-compatible Extension wire form.
use super::types::{SessionEntryData, StoredSessionEntryData};
use crate::model::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};
pub const NATIVE_CONTEXT_NAMESPACE: &str = "aicoder.context-message.v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeContextSource {
    pub extension: String,
    pub hook: String,
    pub kind: String,
}
impl NativeContextSource {
    pub fn message(&self, text: &str) -> Message {
        Message::text(
            MessageRole::System,
            format!(
                "[aicoder extension {}/{}/{}]\n{}",
                self.extension, self.hook, self.kind, text
            ),
        )
    }
    pub fn validate(&self, message: &Message) -> Result<()> {
        for name in [&self.extension, &self.hook, &self.kind] {
            crate::extensions::name(name)?;
        }
        ensure!(
            message.role == MessageRole::System && message.content.len() == 1,
            "invalid native context role/content"
        );
        let text = message.text_content();
        let prefix = self.message("").text_content();
        ensure!(
            text.starts_with(&prefix),
            "native context source does not match message"
        );
        crate::extensions::size(message, 64 * 1024)?;
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u32,
    source: NativeContextSource,
    message: Message,
}
impl SessionEntryData {
    pub fn as_message(&self) -> Option<&Message> {
        match self {
            Self::Message { message } | Self::NativeContext { message, .. } => Some(message),
            _ => None,
        }
    }
    pub fn native_context(source: NativeContextSource, text: &str) -> Result<Self> {
        let message = source.message(text);
        source.validate(&message)?;
        Ok(Self::NativeContext { source, message })
    }
}
impl Serialize for SessionEntryData {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Self::NativeContext { source, message } = self {
            source.validate(message).map_err(S::Error::custom)?;
            let data = serde_json::to_value(Wire {
                version: 1,
                source: source.clone(),
                message: message.clone(),
            })
            .map_err(S::Error::custom)?;
            StoredSessionEntryData::serialize(
                &Self::Extension {
                    namespace: NATIVE_CONTEXT_NAMESPACE.into(),
                    data,
                },
                serializer,
            )
        } else {
            if matches!(self,Self::Extension {namespace,..}if namespace==NATIVE_CONTEXT_NAMESPACE) {
                return Err(S::Error::custom(
                    "native context namespace requires typed NativeContext",
                ));
            }
            StoredSessionEntryData::serialize(self, serializer)
        }
    }
}
impl<'de> Deserialize<'de> for SessionEntryData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = StoredSessionEntryData::deserialize(deserializer)?;
        match value {
            Self::Extension { namespace, data } if namespace == NATIVE_CONTEXT_NAMESPACE => {
                crate::extensions::size(&data, 96 * 1024).map_err(D::Error::custom)?;
                let wire: Wire = serde_json::from_value(data).map_err(D::Error::custom)?;
                ensure_version(wire.version).map_err(D::Error::custom)?;
                wire.source
                    .validate(&wire.message)
                    .map_err(D::Error::custom)?;
                Ok(Self::NativeContext {
                    source: wire.source,
                    message: wire.message,
                })
            }
            other => Ok(other),
        }
    }
}
fn ensure_version(version: u32) -> Result<()> {
    ensure!(version == 1, "unsupported native context version");
    Ok(())
}
