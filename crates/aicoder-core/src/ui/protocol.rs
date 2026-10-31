use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiCapabilities {
    pub notifications: bool,
    pub select: bool,
    pub input: bool,
    pub status: bool,
}
impl UiCapabilities {
    pub fn interactive() -> Self {
        Self {
            notifications: true,
            select: true,
            input: true,
            status: true,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiRequestId {
    pub scope: Uuid,
    pub sequence: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiOption {
    pub id: String,
    pub label: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiPrompt {
    Select {
        title: String,
        options: Vec<UiOption>,
    },
    Input {
        title: String,
        initial: String,
        multiline: bool,
        max_bytes: usize,
    },
}
impl UiPrompt {
    pub(crate) fn valid(&self) -> bool {
        let bounded = match self {
            Self::Select { title, options } => {
                text(title, 512)
                    && !title.trim().is_empty()
                    && (1..=32).contains(&options.len())
                    && {
                        let mut ids = std::collections::BTreeSet::new();
                        options.iter().all(|option| {
                            crate::extensions::name(&option.id).is_ok()
                                && ids.insert(&option.id)
                                && text(&option.label, 256)
                                && !option.label.trim().is_empty()
                        })
                    }
            }
            Self::Input {
                title,
                initial,
                multiline,
                max_bytes,
            } => {
                text(title, 512)
                    && !title.trim().is_empty()
                    && (1..=16384).contains(max_bytes)
                    && text(initial, *max_bytes)
                    && (*multiline || !initial.contains(['\r', '\n']))
            }
        };
        bounded && crate::extensions::size(self, 16 * 1024).is_ok()
    }
    pub(crate) fn accepts(&self, answer: &UiAnswer) -> bool {
        match (self, answer) {
            (_, UiAnswer::Dismissed) => true,
            (Self::Select { options, .. }, UiAnswer::Choice { id }) => {
                options.iter().any(|o| o.id == *id)
            }
            (
                Self::Input {
                    max_bytes,
                    multiline,
                    ..
                },
                UiAnswer::Text { text: answer },
            ) => text(answer, *max_bytes) && (*multiline || !answer.contains(['\r', '\n'])),
            _ => false,
        }
    }
}
pub(crate) fn text(value: &str, max: usize) -> bool {
    value.len() <= max
        && !value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiAnswer {
    Choice { id: String },
    Text { text: String },
    Dismissed,
}
#[derive(Debug, Clone, Serialize)]
pub struct UiQuestion {
    pub id: UiRequestId,
    pub owner: String,
    pub prompt: UiPrompt,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiLevel {
    Info,
    Warning,
    Error,
}
#[derive(Debug, Clone, Serialize)]
pub struct UiNotice {
    pub sequence: u64,
    pub owner: String,
    pub level: UiLevel,
    pub text: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct UiStatus {
    pub owner: String,
    pub key: String,
    pub text: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct UiSnapshot {
    pub version: u32,
    pub revision: u64,
    pub scope: Uuid,
    pub closed: bool,
    pub capabilities: UiCapabilities,
    pub questions: Vec<UiQuestion>,
    pub notices: Vec<UiNotice>,
    pub omitted_notices: u64,
    pub statuses: Vec<UiStatus>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiError {
    Unsupported,
    Closed,
    Cancelled,
    TimedOut,
    Full,
    Invalid,
    Stale,
}
impl fmt::Display for UiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "Host does not support this UI capability",
            Self::Closed => "UI scope is closed",
            Self::Cancelled => "UI request was cancelled",
            Self::TimedOut => "UI request timed out",
            Self::Full => "UI scope admission budget exhausted",
            Self::Invalid => "Invalid UI payload or answer",
            Self::Stale => "Stale or foreign UI request",
        })
    }
}
impl Error for UiError {}
