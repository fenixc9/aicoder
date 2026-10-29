//! Explicit model selection on a frozen service profile. History never selects a route.
use aicoder_core::{ModelProvider, TurnExecutionContext, model::*};
use aicoder_providers::ChatClient;
use async_trait::async_trait;
use std::sync::Arc;
/// Uses registered metadata where available, otherwise the configured protocol profile.
/// This performs no remote model discovery and makes no claim about server availability.
pub struct SessionModelClient {
    client: ChatClient,
}
impl SessionModelClient {
    pub fn new(client: ChatClient) -> Self {
        Self { client }
    }
}
#[async_trait]
impl ModelProvider for SessionModelClient {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        self.client.for_model(model)?.model_spec(model)
    }
    fn validate_request(&self, request: &ModelRequest) -> Result<(), ProviderError> {
        self.client
            .for_model(&request.model)?
            .validate_request(request)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        events: Arc<dyn ModelEventSink>,
        context: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.client
            .for_model(&request.model)?
            .generate(request, events, context)
            .await
    }
}
