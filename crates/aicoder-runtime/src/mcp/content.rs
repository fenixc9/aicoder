use super::*;
use aicoder_core::artifacts::{ArtifactRef, store_bytes};
use base64::Engine;
use std::time::Duration;
fn invalid() -> ToolFailure {
    ToolFailure::new("mcp_invalid_result","MCP request completed with invalid content; inspect the result before repeating the operation").with_details(json!({"operation_completed":true,"delivery_complete":false}))
}
fn copy_fields(source: &Value, target: &mut Value, keys: &[&str]) {
    for key in keys {
        if let Some(value) = source.get(*key) {
            target[*key] = value.clone();
        }
    }
}
async fn media(
    block: &Value,
    data: &str,
    mime: &str,
    context: &ToolContext,
    limit: usize,
    artifacts: &mut Vec<ArtifactRef>,
) -> Result<Value, &'static str> {
    if mime.is_empty() || mime.len() > 128 || !mime.is_ascii() || mime.chars().any(char::is_control)
    {
        return Err("invalid_media_type");
    }
    if block["type"] == "image" && aicoder_core::media::image_media_type(mime).is_none() {
        return Err("invalid_image_mime");
    }
    if data.len() > limit.div_ceil(3).saturating_mul(4) {
        return Err("media_budget");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| "invalid_base64")?;
    if bytes.len() > limit {
        return Err("media_budget");
    }
    let store = context
        .artifact_store()
        .ok_or("artifact_store_unavailable")?;
    let artifact = tokio::time::timeout(
        Duration::from_millis(250),
        store_bytes(store.as_ref(), mime, &bytes, true),
    )
    .await
    .map_err(|_| "artifact_timeout")?
    .map_err(|_| "artifact_failed")?;
    artifact.validate().map_err(|_| "invalid_host_reference")?;
    if !artifact.complete || artifact.bytes != bytes.len() as u64 || artifact.media_type != mime {
        return Err("invalid_host_reference");
    }
    let mut result = json!({"type":block["type"],"mimeType":mime,"artifact":artifact,"encoding":"host_artifact"});
    copy_fields(block, &mut result, &["annotations"]);
    artifacts.push(artifact);
    Ok(result)
}
pub(super) async fn normalize(
    result: &Value,
    context: &ToolContext,
    limits: &McpLimits,
) -> Result<ToolSuccess, ToolFailure> {
    let blocks = match result.get("content") {
        Some(Value::Array(blocks)) if blocks.len() <= limits.max_content_blocks => {
            blocks.as_slice()
        }
        None if result
            .get("structuredContent")
            .is_some_and(Value::is_object) =>
        {
            &[]
        }
        _ => return Err(invalid()),
    };
    if result
        .get("structuredContent")
        .is_some_and(|v| !v.is_object())
        || result.get("isError").is_some_and(|v| !v.is_boolean())
    {
        return Err(invalid());
    }
    let mut content = Vec::new();
    let mut artifacts = Vec::new();
    let mut errors = Vec::new();
    let mut media_bytes = 0usize;
    for (index, block) in blocks.iter().enumerate() {
        let Some(kind) = block.get("type").and_then(Value::as_str) else {
            return Err(invalid());
        };
        let normalized = match kind {
            "text" => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                let mut item = json!({"type":"text","text":text});
                copy_fields(block, &mut item, &["annotations"]);
                Ok(item)
            }
            "image" | "audio" => {
                let data = block
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                let mime = block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                let before = artifacts.len();
                let value = media(
                    block,
                    data,
                    mime,
                    context,
                    limits.max_media_bytes.saturating_sub(media_bytes),
                    &mut artifacts,
                )
                .await;
                if artifacts.len() > before {
                    media_bytes = media_bytes
                        .saturating_add(artifacts.last().expect("stored media").bytes as usize);
                }
                value
            }
            "resource_link" => {
                if block
                    .get("uri")
                    .and_then(Value::as_str)
                    .is_none_or(|s| s.len() > 4096)
                    || !block.get("name").is_some_and(Value::is_string)
                {
                    return Err(invalid());
                }
                let mut item = json!({"type":"resource_link"});
                copy_fields(
                    block,
                    &mut item,
                    &[
                        "uri",
                        "name",
                        "title",
                        "description",
                        "mimeType",
                        "size",
                        "annotations",
                    ],
                );
                Ok(item)
            }
            "resource" => {
                let resource = block
                    .get("resource")
                    .filter(|r| r.is_object())
                    .ok_or_else(invalid)?;
                let uri = resource
                    .get("uri")
                    .and_then(Value::as_str)
                    .filter(|s| s.len() <= 4096)
                    .ok_or_else(invalid)?;
                if resource.get("text").is_some() == resource.get("blob").is_some() {
                    return Err(invalid());
                }
                if let Some(text) = resource.get("text").and_then(Value::as_str) {
                    let mut item = json!({"type":"resource","resource":{"uri":uri,"text":text}});
                    copy_fields(resource, &mut item["resource"], &["mimeType"]);
                    copy_fields(block, &mut item, &["annotations"]);
                    Ok(item)
                } else {
                    let data = resource
                        .get("blob")
                        .and_then(Value::as_str)
                        .ok_or_else(invalid)?;
                    let mime = resource
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("application/octet-stream");
                    let before = artifacts.len();
                    let value = media(
                        block,
                        data,
                        mime,
                        context,
                        limits.max_media_bytes.saturating_sub(media_bytes),
                        &mut artifacts,
                    )
                    .await
                    .map(|mut item| {
                        item["uri"] = Value::String(uri.into());
                        item
                    });
                    if artifacts.len() > before {
                        media_bytes = media_bytes
                            .saturating_add(artifacts.last().expect("stored media").bytes as usize);
                    }
                    value
                }
            }
            _ => Err("unsupported_content_type"),
        };
        match normalized {
            Ok(item) => content.push(item),
            Err(code) => {
                errors.push(json!({"index":index,"code":code}));
                content.push(json!({"type":kind,"delivery_complete":false,"capture_error":code,"notice":"This MCP request completed; do not repeat it to recover missing media."}));
            }
        }
    }
    let complete = errors.is_empty();
    let mut output = json!({"content":content,"operation_completed":true,"delivery_complete":complete,"capture_errors":errors});
    if let Some(structured) = result.get("structuredContent") {
        output["structuredContent"] = structured.clone();
    }
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let mut error = ToolFailure::new(
            "mcp_tool_error",
            "MCP tool reported an error; inspect its completed result before retrying",
        )
        .with_details(output);
        error.artifacts = artifacts;
        return Err(error);
    }
    Ok(ToolSuccess {
        output,
        truncated: !complete,
        artifacts,
    })
}
