//! Explicit data-only media output; authority comes from the installed scoped store and tool.
use super::{
    ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess, io::*,
    util::parse_arguments,
};
use aicoder_core::{artifacts::*, model::ToolDefinition, tool_images::MAX_TOOL_IMAGE_SOURCE_BYTES};
use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

pub struct ImageOutputTool;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    artifact_id: Option<String>,
    data: Option<String>,
    media_type: Option<String>,
}
fn capture_failed(code: &str) -> ToolFailure {
    failure("image_output_capture", "Image data was supplied but storage delivery failed; completed operations were not repeated")
        .with_details(json!({"operation_completed":true,"delivery_complete":false,"capture_error":code}))
}
#[async_trait]
impl ExecutableTool for ImageOutputTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name:aicoder_codemode::IMAGE_OUTPUT_TOOL.into(), description:Some("Output one image from supplied base64 data/media_type or an existing scoped artifact_id. No files or URLs are fetched. Code Mode image() explicitly selects this output for the next model view; normal tool permission and budgets apply.".into()),
            parameters:json!({"type":"object","properties":{"artifact_id":{"type":"string","maxLength":36},"data":{"type":"string","maxLength":MAX_TOOL_IMAGE_SOURCE_BYTES.div_ceil(3)*4},"media_type":{"type":"string","maxLength":128}},"additionalProperties":false}) }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        bounded(context, output(context, parse_arguments(arguments)?)).await
    }
}
async fn output(context: &ToolContext, args: Arguments) -> Result<ToolSuccess, ToolFailure> {
    let limit = page_limit(context)?;
    let store = context.artifact_store().ok_or_else(|| {
        failure(
            "artifact_store_unavailable",
            "Image output requires explicit scoped artifact storage",
        )
    })?;
    let reference = match (args.artifact_id, args.data, args.media_type) {
        (Some(id), None, None) => {
            if !valid_id(&id) {
                return Err(failure(
                    "invalid_image_reference",
                    "Image output requires a canonical artifact ID",
                ));
            }
            let page = tokio::time::timeout(Duration::from_millis(250), store.read(&id, 0, 1))
                .await
                .map_err(|_| failure("image_output_timeout", "Scoped image lookup timed out"))?
                .map_err(|_| {
                    failure(
                        "image_output_unavailable",
                        "Image is unavailable in this host scope",
                    )
                })?;
            let reference = page.artifact;
            let count = reference.bytes.min(1) as usize;
            if reference.validate().is_err()
                || reference.id != id
                || !reference.is_image()
                || reference.bytes == 0
                || reference.bytes > MAX_TOOL_IMAGE_SOURCE_BYTES
                || page.offset != 0
                || page.bytes.len() != count
                || page.next_offset != (reference.bytes > 1).then_some(1)
            {
                return Err(failure(
                    "invalid_image_reference",
                    "Scoped storage did not return a valid bounded image reference",
                ));
            }
            reference
        }
        (None, Some(data), Some(mime)) => {
            if aicoder_core::media::image_media_type(&mime).is_none() {
                return Err(failure(
                    "invalid_image_mime",
                    "Image output requires a bounded image MIME declaration",
                ));
            }
            let maximum =
                MAX_TOOL_IMAGE_SOURCE_BYTES.min(store.limits().max_artifact_bytes) as usize;
            if data.len() > maximum.div_ceil(3) * 4 {
                return Err(failure(
                    "image_source_limit",
                    "Inline image exceeds its byte admission budget",
                ));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&data)
                .map_err(|_| failure("invalid_base64", "Image data must be canonical base64"))?;
            if bytes.is_empty() || bytes.len() > maximum {
                return Err(failure(
                    "image_source_limit",
                    "Inline image exceeds its byte admission budget",
                ));
            }
            // Admit maximum reference metadata before publication; no preview bytes enter history.
            let placeholder = ArtifactRef {
                version: 1,
                id: "00000000-0000-0000-0000-000000000000".into(),
                media_type: mime.clone(),
                bytes: bytes.len() as u64,
                complete: true,
            };
            success(
                json!({"kind":"image","artifact":placeholder,"operation_completed":true,"delivery_complete":true}),
                limit,
            )?;
            let reference = tokio::time::timeout(
                Duration::from_millis(250),
                store_bytes(store.as_ref(), &mime, &bytes, true),
            )
            .await
            .map_err(|_| capture_failed("storage_timeout"))?
            .map_err(|_| capture_failed("storage_failed"))?;
            if reference.validate().is_err()
                || !reference.complete
                || reference.media_type != mime
                || reference.bytes != bytes.len() as u64
            {
                return Err(capture_failed("invalid_host_reference"));
            }
            reference
        }
        _ => {
            return Err(failure(
                "invalid_arguments",
                "Supply only artifact_id or data with media_type",
            ));
        }
    };
    let mut result = success(
        json!({"kind":"image","artifact":reference,"operation_completed":true,"delivery_complete":true}),
        limit,
    )?;
    result.artifacts.push(reference);
    Ok(result)
}
