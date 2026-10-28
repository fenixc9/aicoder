//! Model service adapters for aicoder. HTTP and service configuration live here, not in core.
//!
//! The first adapter implements the existing OpenAI-compatible completion boundary.
//! Protocol-neutral model types are the next S1 migration step.

mod chat;
mod stream;

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
