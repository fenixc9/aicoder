//! An in-memory model provider with no HTTP client, workspace or process configuration.
use std::sync::Arc;

use aicoder_core::{TurnExecutionContext, model::*};
use async_trait::async_trait;

struct EchoProvider;

#[async_trait]
impl ModelProvider for EchoProvider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        if model != "echo" {
            return Err(ProviderError::new(
                "memory",
                ProviderErrorKind::ModelNotFound,
                "unknown model",
            ));
        }
        Ok(ModelSpec::new("memory", model))
    }

    async fn generate(
        &self,
        request: ModelRequest,
        events: Arc<dyn ModelEventSink>,
        context: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        if let Err(error) = self
            .model_spec(&request.model)
            .and_then(|spec| request.validate(&spec))
        {
            events.emit(ModelEvent::Aborted {
                message: error.to_string(),
            });
            return Err(error);
        }
        if context.is_cancelled() {
            let error =
                ProviderError::new("memory", ProviderErrorKind::Cancelled, context.reason());
            events.emit(ModelEvent::Aborted {
                message: error.to_string(),
            });
            return Err(error);
        }
        let text = request.messages.last().unwrap().text_content();
        events.emit(ModelEvent::Started);
        events.emit(ModelEvent::TextDelta {
            delta: text.clone(),
        });
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, text),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::default(),
        };
        events.emit(ModelEvent::Usage {
            usage: response.usage.clone(),
        });
        events.emit(ModelEvent::Finished {
            stop_reason: response.stop_reason.clone(),
        });
        Ok(response)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ProviderError> {
    let provider: Arc<dyn ModelProvider> = Arc::new(EchoProvider);
    let request = ModelRequest::new(
        "echo",
        vec![Message::text(
            MessageRole::User,
            "hello from a custom provider",
        )],
    );
    let response = provider
        .generate(request, Arc::new(()), TurnExecutionContext::new())
        .await?;
    assert_eq!(response.usage.total_tokens, None);
    println!("{}", response.message.text_content());
    Ok(())
}
