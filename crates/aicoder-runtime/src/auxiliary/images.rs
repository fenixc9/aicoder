use super::*;
use aicoder_core::artifacts::{ArtifactRef, ArtifactWriter};
use std::time::Duration;
pub const MODEL_GENERATE_IMAGES_TOOL: &str = "model_generate_images";
pub(super) struct Entry {
    pub model: ImageGenerationModel,
    pub provider: Arc<dyn ImageGenerationProvider>,
    pub available: bool,
}
impl Entry {
    pub fn metadata(&self) -> Value {
        let m = &self.model.specification;
        json!({"type":"image","provider":m.provider,"id":m.id,"name":m.name,"input":if m.images{vec!["text","image"]}else{vec!["text"]},"output":if self.model.text_output{vec!["image","text"]}else{vec!["image"]},"contextWindow":m.context_tokens,"api":"openrouter_images","available":self.available})
    }
}
pub(super) fn register(catalog: Arc<Catalog>, registry: &mut ToolRegistry) -> Result<()> {
    if !catalog.images.is_empty() {
        registry.register(GenerateTool(catalog))?;
    }
    Ok(())
}
struct GenerateTool(Arc<Catalog>);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    model: Selector,
    context: ImageGenerationContext,
}
pub struct PreparedImageGeneration {
    model: ImageGenerationModel,
    context: ImageGenerationContext,
    provider: Arc<dyn ImageGenerationProvider>,
    preview: ToolPreview,
}
#[async_trait]
impl ExecutableTool for GenerateTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name:MODEL_GENERATE_IMAGES_TOOL.into(),description:Some("Generate images from text and optional captured image references using an explicit host model. Shared parent budgets/approvals apply. Results are scoped artifacts; no URLs are fetched and completed inference is never implicitly repeated.".into()),parameters:json!({"type":"object","properties":{"model":{"type":"object","properties":{"provider":{"type":"string"},"id":{"type":"string"}},"required":["provider","id"],"additionalProperties":false},"context":{"type":"object","properties":{"input":{"type":"array","minItems":1,"maxItems":32}},"required":["input"],"additionalProperties":false}},"required":["model","context"],"additionalProperties":false}) }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Command
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    async fn prepare(
        &self,
        tool: &ToolContext,
        args: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        let call: Call = serde_json::from_value(args)
            .map_err(|_| ToolFailure::new("invalid_arguments", "Invalid image model/context"))?;
        let entry = self
            .0
            .images
            .iter()
            .find(|m| {
                m.model.specification.provider == call.model.provider
                    && m.model.specification.id == call.model.id
            })
            .ok_or_else(|| {
                ToolFailure::new(
                    "image_model_not_available",
                    "Image model is not in the selected host catalog",
                )
            })?;
        let store = tool.artifact_store().ok_or_else(|| {
            ToolFailure::new(
                "artifact_store_unavailable",
                "Image generation requires explicit scoped output storage",
            )
        })?;
        if store.limits().validate().is_err() || tool.max_output_bytes() < 512 {
            return Err(ToolFailure::new(
                "image_delivery_admission",
                "Image output storage/budget cannot admit a result",
            ));
        }
        let mut context = call.context;
        context.validate(&entry.model).map_err(|_| {
            ToolFailure::new(
                "invalid_arguments",
                "Invalid bounded image generation input",
            )
        })?;
        for block in &mut context.input {
            if let ImageGenerationInput::Image { source } = block {
                resolve_source(source, tool).await?;
            }
        }
        context.validate(&entry.model).map_err(|_| {
            ToolFailure::new("invalid_arguments", "Captured image input exceeds bounds")
        })?;
        let input = context
            .input
            .iter()
            .map(|block| match block {
                ImageGenerationInput::Text { text } => json!({"type":"text","text":text}),
                ImageGenerationInput::Image { .. } => json!({"type":"image","captured":true}),
            })
            .collect::<Vec<_>>();
        let mut body=json!({"provider":entry.model.specification.provider,"model":entry.model.specification.id,"input":input}).to_string();
        let truncated = body.len() > 24000;
        if truncated {
            let mut at = 24000;
            while !body.is_char_boundary(at) {
                at -= 1;
            }
            body.truncate(at);
        }
        Ok(Some(Box::new(PreparedImageGeneration {
            model: entry.model.clone(),
            context,
            provider: entry.provider.clone(),
            preview: ToolPreview {
                summary: format!(
                    "Generate images {}/{}",
                    entry.model.specification.provider, entry.model.specification.id
                ),
                body,
                format: "json".into(),
                truncated,
            },
        })))
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        Err(unmetered())
    }
}
fn unmetered() -> ToolFailure {
    ToolFailure::new(
        "auxiliary_scope_required",
        "Image generation requires its owned metered plan",
    )
}
#[async_trait]
impl PreparedTool for PreparedImageGeneration {
    fn preview(&self) -> ToolPreview {
        self.preview.clone()
    }
    fn image_generation_operation(&self) -> Option<&dyn ImageGenerationOperation> {
        Some(self)
    }
    async fn execute(self: Box<Self>, _: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        Err(unmetered())
    }
}
#[async_trait]
impl ImageGenerationOperation for PreparedImageGeneration {
    fn model(&self) -> &ImageGenerationModel {
        &self.model
    }
    fn context(&self) -> &ImageGenerationContext {
        &self.context
    }
    fn provider(&self) -> &dyn ImageGenerationProvider {
        self.provider.as_ref()
    }
    async fn deliver(
        &self,
        output: Vec<GeneratedBlock>,
        usage: Option<ModelUsage>,
        context: &ToolContext,
    ) -> Result<ToolSuccess, ToolFailure> {
        let mut receipts = Vec::<ArtifactRef>::new();
        let failure = |code: &str, receipts: &[ArtifactRef]| {
            ToolFailure::new("image_generation_delivery","Image inference completed but output delivery failed; inference was not retried")
            .with_details(json!({"operation_completed":true,"delivery_complete":false,"capture_error":code,"captured_artifacts":receipts}))
        };
        let store = context
            .artifact_store()
            .ok_or_else(|| failure("storage_unavailable", &receipts))?;
        let mut blocks = Vec::new();
        for block in &output {
            blocks.push(match block {
                GeneratedBlock::Text{text}=>json!({"type":"text","text":text}),
                GeneratedBlock::Image{media_type,bytes}=>json!({"type":"image","artifact":ArtifactRef {version:1,id:"00000000-0000-0000-0000-000000000000".into(),media_type:media_type.clone(),bytes:bytes.len() as u64,complete:true}}),
            });
        }
        let mut result = json!({"provider":self.model.specification.provider,"model":self.model.specification.id,"output":blocks,"usage":usage,"stopReason":"stop"});
        if result.to_string().len() > context.max_output_bytes() {
            return Err(failure("output_limit", &receipts));
        }
        let work = async {
            let mut staged: Vec<(usize, String, usize, Box<dyn ArtifactWriter>)> = Vec::new();
            for (index, block) in output.into_iter().enumerate() {
                if let GeneratedBlock::Image { media_type, bytes } = block {
                    let mut writer = store
                        .begin(&media_type)
                        .await
                        .map_err(|_| failure("storage_admission", &receipts))?;
                    for chunk in bytes.chunks(16384) {
                        writer
                            .write(chunk)
                            .await
                            .map_err(|_| failure("storage_write", &receipts))?;
                    }
                    staged.push((index, media_type, bytes.len(), writer));
                }
            }
            // Admit/stage the entire set before making any image visible.
            for (index, mime, size, writer) in staged {
                let reference = writer
                    .finish(true)
                    .await
                    .map_err(|_| failure("storage_publish", &receipts))?;
                let valid = reference.validate().is_ok()
                    && reference.media_type == mime
                    && reference.bytes == size as u64
                    && reference.complete;
                receipts.push(reference.clone());
                if !valid {
                    return Err(failure("invalid_host_reference", &receipts));
                }
                result["output"][index]["artifact"] = json!(reference);
            }
            Ok(())
        };
        // Outer cancellation drops staged writers; publication uncertainty is never retried.
        match tokio::time::timeout(context.tool_timeout().min(Duration::from_secs(5)), work).await {
            Ok(result) => result?,
            Err(_) => return Err(failure("storage_timeout", &receipts)),
        }
        let mut success = ToolSuccess::new(result);
        success.artifacts = receipts;
        Ok(success)
    }
}
