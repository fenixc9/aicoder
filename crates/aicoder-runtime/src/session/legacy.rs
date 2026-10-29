//! Private v1 storage codec. Legacy transport types are deliberately not a core dependency.
use aicoder_core::model::{ContentBlock, Message, MessageRole, ModelToolCall};
use anyhow::{Context, Result};
use serde_json::Value;

pub(super) fn decode(value: Value) -> Result<Message> {
    let role: MessageRole =
        serde_json::from_value(value["role"].clone()).context("invalid v1 role")?;
    let mut content = Vec::new();
    if role == MessageRole::Tool {
        let text = value["content"].as_str().unwrap_or_default();
        let output =
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.into()));
        content.push(ContentBlock::ToolResult {
            call_id: value["tool_call_id"]
                .as_str()
                .context("v1 tool result missing ID")?
                .into(),
            is_error: output["ok"] == false,
            content: output,
        });
    } else {
        if let Some(text) = value["reasoning"].as_str() {
            content.push(ContentBlock::Reasoning { text: text.into() });
        }
        if let Some(text) = value["content"].as_str() {
            content.push(ContentBlock::Text { text: text.into() });
        }
        if let Some(calls) = value["tool_calls"].as_array() {
            for call in calls {
                content.push(ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: call["id"].as_str().context("v1 call missing ID")?.into(),
                        name: call["function"]["name"]
                            .as_str()
                            .context("v1 call missing name")?
                            .into(),
                        arguments: serde_json::from_str(
                            call["function"]["arguments"]
                                .as_str()
                                .context("v1 call missing arguments")?,
                        )
                        .context("invalid v1 tool arguments")?,
                    },
                });
            }
        }
    }
    Ok(Message { role, content })
}
