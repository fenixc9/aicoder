//! Private v1 storage codec. Legacy transport types are deliberately not a core dependency.
use crate::model::{ContentBlock, Message, MessageRole, ModelToolCall};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

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

/// Preserve the original file format when continuing a v1 conversation. New sessions use v2.
pub(super) fn encode(message: &Message) -> Result<Value> {
    let mut value = json!({"role":message.role,"content":null});
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text {text:part} => text.push_str(part),
            ContentBlock::Reasoning {text} => reasoning.push_str(text),
            ContentBlock::ToolCall {call} => calls.push(json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments.to_string()}})),
            ContentBlock::ToolResult {call_id,content,is_error} if message.content.len()==1 => {
                anyhow::ensure!(*is_error==(content["ok"]==false), "v1 cannot preserve this tool error flag; create a v2 session");
                value["tool_call_id"]=json!(call_id);value["content"]=json!(content.to_string());return Ok(value);
            }
            _ => bail!("v1 sessions cannot store images, opaque state or mixed tool results; create a v2 session"),
        }
    }
    if !text.is_empty() || calls.is_empty() {
        value["content"] = json!(text);
    }
    if !reasoning.is_empty() {
        value["reasoning"] = json!(reasoning);
    }
    if !calls.is_empty() {
        value["tool_calls"] = json!(calls);
    }
    anyhow::ensure!(
        decode(value.clone())? == *message,
        "v1 cannot preserve this content block order; create a v2 session"
    );
    Ok(value)
}
