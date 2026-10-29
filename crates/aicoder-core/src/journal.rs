//! Optional durable checkpoints, independent of storage and presentation.
use crate::{RunOutcome, model::Message};
use async_trait::async_trait;
#[async_trait]
pub trait RunJournal: Send + Sync {
    /// Index is a logical transcript position. Tool results reserve positions in original call order.
    async fn checkpoint(&self, index: usize, message: &Message) -> anyhow::Result<()>;
    /// Persist an applied user control, optionally with its provenance, at this transcript position.
    async fn input(&self, index: usize, input: &crate::RunInput) -> anyhow::Result<()> {
        self.checkpoint(index, &input.message).await
    }
    /// Persist source metadata before the first model call; defaults to no additional storage.
    async fn record_input_context(&self, _provenance: &serde_json::Value) -> anyhow::Result<()> {
        Ok(())
    }
    /// Persist a summary before its reduced context reaches the next model call.
    /// Retained messages form an exact canonical suffix. None means a transient host.
    async fn record_summary(
        &self,
        _draft: &crate::summary::SummaryDraft,
        _input_messages: usize,
        _history_messages: usize,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
    /// Called before terminal run events. Must retain the stop even when execution was cancelled.
    async fn finish(&self, outcome: &RunOutcome) -> anyhow::Result<()>;
}
#[async_trait]
impl RunJournal for () {
    async fn checkpoint(&self, _: usize, _: &Message) -> anyhow::Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> anyhow::Result<()> {
        Ok(())
    }
}
