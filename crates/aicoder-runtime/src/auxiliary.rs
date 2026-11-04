//! Trusted model catalogs and immutable prepared non-chat operations.
mod images;
use crate::*;
use aicoder_core::{auxiliary::*, model::*, tools::*};
use aicoder_providers::auxiliary::SystemOneClient;
use async_trait::async_trait;
pub use images::{MODEL_GENERATE_IMAGES_TOOL, PreparedImageGeneration};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
pub const MODEL_CATALOG_TOOL: &str = "model_catalog";
pub const MODEL_CLASSIFY_TOOL: &str = "model_classify";
struct Entry {
    model: ClassifierModel,
    provider: Arc<dyn ClassifierProvider>,
    available: bool,
}
struct Catalog {
    entries: Vec<Entry>,
    chat: Vec<Value>,
    images: Vec<images::Entry>,
}
impl Runtime {
    pub(crate) fn register_auxiliary_tools(&self, registry: &mut ToolRegistry) -> Result<()> {
        if !self.config.code_mode.enabled && self.config.classifier_routes.is_empty() {
            return Ok(());
        }
        let mut entries = Vec::new();
        let mut image_entries = Vec::new();
        for route in &self.config.classifier_routes {
            let variable = route
                .config
                .api_key_env
                .clone()
                .context("classifier credential reference is missing")?;
            let available = self.environment.present(&variable)?;
            let credentials = Arc::new(RuntimeCredential {
                environment: self.environment.clone(),
                variable,
                provider: route.config.provider_id.clone(),
            });
            if !route.models.is_empty() {
                let mut provider = SystemOneClient::new(
                    route.config.clone(),
                    credentials.clone(),
                    route.models.clone(),
                    route.images,
                )?;
                if let Some(binding) = &self.model_transport {
                    provider = provider.with_transport_binding(binding.clone());
                }
                let provider = Arc::new(provider);
                for model in &route.models {
                    entries.push(Entry {
                        model: model.clone(),
                        provider: provider.clone(),
                        available,
                    });
                }
            }
            if !route.image_models.is_empty() {
                let mut provider = aicoder_providers::image_generation::OpenRouterImageClient::new(
                    route.config.clone(),
                    credentials,
                    route.image_models.clone(),
                )?;
                if let Some(binding) = &self.model_transport {
                    provider = provider.with_transport_binding(binding.clone());
                }
                let provider = Arc::new(provider);
                for model in &route.image_models {
                    image_entries.push(images::Entry {
                        model: model.clone(),
                        provider: provider.clone(),
                        available,
                    });
                }
            }
        }
        anyhow::ensure!(
            entries.len() + image_entries.len() <= 128,
            "auxiliary catalog exceeds 128 entries"
        );
        let mut chat = Vec::new();
        let available = self
            .config
            .provider
            .api_key_env
            .as_ref()
            .is_some_and(|name| self.environment.present(name).unwrap_or(false));
        for model in &self.config.models {
            chat.push(json!({"type":"chat","provider":model.provider,"id":model.id,"name":model.id,"input":if model.capabilities.images{vec!["text","image"]}else{vec!["text"]},"available":available}));
        }
        if !chat.iter().any(|m| {
            m["provider"] == self.config.provider.provider_id
                && m["id"] == self.config.provider.model
        }) {
            chat.push(json!({"type":"chat","provider":self.config.provider.provider_id,"id":self.config.provider.model,"name":self.config.provider.model,"input":["text"],"available":available}));
        }
        let catalog = Arc::new(Catalog {
            entries,
            chat,
            images: image_entries,
        });
        registry.register(CatalogTool(catalog.clone()))?;
        if !catalog.entries.is_empty() {
            registry.register(ClassifyTool(catalog.clone()))?;
        }
        images::register(catalog, registry)?;
        Ok(())
    }
}
struct CatalogTool(Arc<Catalog>);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    #[serde(rename = "type")]
    kind: Option<String>,
    provider: Option<String>,
    #[serde(default)]
    available: bool,
}
#[async_trait]
impl ExecutableTool for CatalogTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition{name:MODEL_CATALOG_TOOL.into(),description:Some("List the current host model catalog by type/provider and configured credential availability. Metadata grants no route or tool permission.".into()),parameters:json!({"type":"object","properties":{"type":{"enum":["chat","classifier","image"]},"provider":{"type":"string","maxLength":128},"available":{"type":"boolean"}},"additionalProperties":false})}
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        let query: Query = serde_json::from_value(args)
            .map_err(|_| ToolFailure::new("invalid_arguments", "Invalid model catalog query"))?;
        if query
            .kind
            .as_ref()
            .is_some_and(|kind| !matches!(kind.as_str(), "chat" | "classifier" | "image"))
        {
            return Err(ToolFailure::new("invalid_arguments", "Unknown model type"));
        }
        let mut models = self.0.chat.clone();
        models.extend(self.0.entries.iter().map(|entry|json!({"type":"classifier","provider":entry.model.provider,"id":entry.model.id,"name":entry.model.name,"input":if entry.model.images{vec!["text","image"]}else{vec!["text"]},"contextWindow":entry.model.context_tokens,"api":"system_one","available":entry.available})));
        models.extend(self.0.images.iter().map(images::Entry::metadata));
        models.retain(|m| {
            query.kind.as_ref().is_none_or(|kind| m["type"] == *kind)
                && query
                    .provider
                    .as_ref()
                    .is_none_or(|provider| m["provider"] == *provider)
                && (!query.available || m["available"] == true)
        });
        let output = json!(models);
        if output.to_string().len() > context.max_output_bytes() {
            return Err(ToolFailure::new(
                "output_limit_too_small",
                "Model catalog requires a larger output budget",
            ));
        }
        Ok(ToolSuccess::new(output))
    }
}
struct ClassifyTool(Arc<Catalog>);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selector {
    provider: String,
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    model: Selector,
    context: ClassifierContext,
}
pub struct PreparedClassification {
    model: ClassifierModel,
    context: ClassifierContext,
    provider: Arc<dyn ClassifierProvider>,
    preview: ToolPreview,
}
#[async_trait]
impl ExecutableTool for ClassifyTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition{name:MODEL_CLASSIFY_TOOL.into(),description:Some("Answer bounded typed choice/score/bool questions about JSON state using an explicitly configured non-chat classifier. Provider/model selects only the current host catalog. Requests are metered, cancellable and not implicitly retried.".into()),parameters:json!({"type":"object","properties":{"model":{"type":"object","properties":{"provider":{"type":"string"},"id":{"type":"string"}},"required":["provider","id"],"additionalProperties":false},"context":{"type":"object","properties":{"state":{"type":"object"},"questions":{"type":"object"},"images":{"type":"array","maxItems":4}},"required":["state","questions"]}},"required":["model","context"],"additionalProperties":false})}
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Command
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    async fn prepare(
        &self,
        tool_context: &ToolContext,
        args: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        let call: Call = serde_json::from_value(args).map_err(|_| {
            ToolFailure::new("invalid_arguments", "Invalid classifier model/context")
        })?;
        let entry = self
            .0
            .entries
            .iter()
            .find(|entry| {
                entry.model.provider == call.model.provider && entry.model.id == call.model.id
            })
            .ok_or_else(|| {
                ToolFailure::new(
                    "classifier_not_available",
                    "Classifier is not in the selected host catalog",
                )
            })?;
        let mut context = call.context;
        context.validate(&entry.model).map_err(|_| {
            ToolFailure::new("invalid_arguments", "Invalid bounded classifier input")
        })?;
        for source in &mut context.images {
            resolve_source(source, tool_context).await?;
        }
        context.validate(&entry.model).map_err(|_| {
            ToolFailure::new(
                "invalid_arguments",
                "Captured classifier image exceeds bounds",
            )
        })?;
        let mut preview=json!({"provider":entry.model.provider,"model":entry.model.id,"state":context.state,"questions":context.questions,"images":context.images.len()}).to_string();
        let truncated = preview.len() > 24000;
        if truncated {
            let mut at = 24000;
            while !preview.is_char_boundary(at) {
                at -= 1;
            }
            preview.truncate(at);
        }
        Ok(Some(Box::new(PreparedClassification {
            model: entry.model.clone(),
            context,
            provider: entry.provider.clone(),
            preview: ToolPreview {
                summary: format!("Run classifier {}/{}", entry.model.provider, entry.model.id),
                body: preview,
                format: "json".into(),
                truncated,
            },
        })))
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        Err(ToolFailure::new(
            "auxiliary_scope_required",
            "Classifier execution requires its owned metered plan",
        ))
    }
}
#[async_trait]
impl PreparedTool for PreparedClassification {
    fn preview(&self) -> ToolPreview {
        self.preview.clone()
    }
    fn auxiliary_operation(&self) -> Option<&dyn AuxiliaryOperation> {
        Some(self)
    }
    async fn execute(self: Box<Self>, _: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        Err(ToolFailure::new(
            "auxiliary_scope_required",
            "Classifier plan needs the metered Agent dispatcher",
        ))
    }
}
#[async_trait]
impl AuxiliaryOperation for PreparedClassification {
    fn model(&self) -> &ClassifierModel {
        &self.model
    }
    fn context(&self) -> &ClassifierContext {
        &self.context
    }
    fn provider(&self) -> &dyn ClassifierProvider {
        self.provider.as_ref()
    }
    async fn deliver(
        &self,
        answers: ClassifierAnswers,
        usage: Option<ModelUsage>,
        context: &ToolContext,
    ) -> Result<ToolSuccess, ToolFailure> {
        let output = json!({"provider":self.model.provider,"model":self.model.id,"answers":answers,"usage":usage,"stopReason":"stop"});
        if output.to_string().len() > context.max_output_bytes() {
            return Err(ToolFailure::new("auxiliary_delivery_limit","Completed classifier result exceeds the complete output budget; the request was not retried").with_details(json!({"operation_completed":true,"delivery_complete":false})));
        }
        Ok(ToolSuccess::new(output))
    }
}

async fn resolve_source(
    source: &mut ImageSource,
    tool_context: &ToolContext,
) -> Result<(), ToolFailure> {
    if let ImageSource::Artifact { reference } = source {
        #[cfg(feature = "images")]
        {
            use aicoder_core::tool_images::ToolImageResolver;
            let store = tool_context.artifact_store().ok_or_else(|| {
                ToolFailure::new(
                    "artifact_store_unavailable",
                    "Image input needs current host storage",
                )
            })?;
            let image = crate::images::ArtifactImageResolver::new(store.clone())
                .resolve(reference, aicoder_core::TurnExecutionContext::new())
                .await
                .map_err(|_| ToolFailure::new("auxiliary_image", "Image could not be captured"))?;
            *source = image.source().clone();
        }
        #[cfg(not(feature = "images"))]
        {
            let _ = (reference, tool_context);
            return Err(ToolFailure::new(
                "auxiliary_image",
                "Artifact image input requires the images feature",
            ));
        }
    }
    Ok(())
}
