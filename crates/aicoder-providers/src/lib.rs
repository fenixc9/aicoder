//! Model service adapters for aicoder. HTTP and service configuration live here, not in core.
//!
//! The OpenAI-compatible adapter implements both the provider-neutral model contract
//! and the legacy completion boundary while the agent loop migrates to the new API.

mod chat;
mod event_sink;
mod model;
mod stream;

pub(crate) use event_sink::ProviderEventSink;

pub use chat::{ChatClient, ClientConfig};

use aicoder_core::{
    ChatCompletionProvider, TurnExecutionContext,
    events::{AgentEventEmitter, emit_full_response_events},
    types::{ChatCompletionRequest, ChatCompletionResponse},
};
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
impl ChatCompletionProvider for ChatClient {
    async fn complete(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        if request.stream.unwrap_or(false) {
            self.chat_completion_stream_collect(request).await
        } else {
            self.chat_completion(request).await
        }
    }

    async fn complete_with_events(
        &self,
        request: ChatCompletionRequest,
        events: AgentEventEmitter,
    ) -> Result<ChatCompletionResponse> {
        if request.stream.unwrap_or(false) {
            self.chat_completion_stream_collect_with_events(request, &events)
                .await
        } else {
            let response = self.chat_completion_with_events(request, &events).await?;
            emit_full_response_events(&events, &response);
            Ok(response)
        }
    }

    async fn complete_with_context(
        &self,
        request: ChatCompletionRequest,
        events: AgentEventEmitter,
        context: &TurnExecutionContext,
    ) -> Result<ChatCompletionResponse> {
        if request.stream.unwrap_or(false) {
            self.chat_completion_stream_collect_with_context(request, &events, context)
                .await
        } else {
            tokio::select! {
                biased;
                cancelled = context.cancelled() => Err(cancelled.into()),
                response = self.chat_completion_with_events(request, &events) => response,
            }
        }
    }
}
