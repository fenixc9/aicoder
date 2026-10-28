//! Optional durable checkpoints, independent of storage and presentation.
use crate::{RunOutcome, model::Message};
use async_trait::async_trait;
#[async_trait]
pub trait RunJournal: Send + Sync {
    /// Index is a logical transcript position. Tool results reserve positions in original call order.
    async fn checkpoint(&self, index: usize, message: &Message) -> anyhow::Result<()>;
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
