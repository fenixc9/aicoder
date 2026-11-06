//! Optional durable checkpoints, independent of storage and presentation.
use crate::{RunOutcome, model::Message};
use async_trait::async_trait;
#[async_trait]
pub trait RunJournal: Send + Sync {
    async fn record_message_finalization(
        &self,
        _record: &crate::extensions::MessageFinalizationRecord,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support message finalization")
    }
    async fn finalize_message(
        &self,
        _commit: &crate::extensions::MessageFinalizationCommit,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support atomic message finalization")
    }
    /// Index is a logical transcript position. Tool results reserve positions in original call order.
    async fn checkpoint(&self, index: usize, message: &Message) -> anyhow::Result<()>;
    /// Probe/record installed finalizers before provider IO. Unsupported journals fail closed.
    async fn record_finalization(
        &self,
        _record: &crate::extensions::FinalizationRecord,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support assistant finalization")
    }
    /// Atomically persist the receipt and effective assistant at an unsaved canonical position.
    /// Successful acknowledgement must prevent `finish`/`checkpoint` from duplicating it.
    async fn finalize_assistant(
        &self,
        _index: usize,
        _record: &crate::extensions::FinalizationRecord,
        _message: &Message,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support atomic assistant finalization")
    }
    fn supports_custom_input(&self) -> bool {
        false
    }
    async fn input_custom(&self, _index: usize, _input: &crate::RunInput) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support custom input")
    }
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
    /// Record real nested operation facts without changing transcript checkpoint indices.
    async fn record_composition(
        &self,
        _record: &crate::composition::CompositionRecord,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support canonical tool composition records")
    }
    async fn record_input_hooks(
        &self,
        _record: &crate::extensions::InputHookRecord,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support input hook records")
    }
    /// Atomically record raw provenance and the actual effective queued user input.
    async fn input_prepared(
        &self,
        _index: usize,
        _input: &crate::RunInput,
        _receipt: &crate::extensions::InputReceipt,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support prepared input")
    }
    async fn record_lifecycle(
        &self,
        _record: &crate::extensions::LifecycleRecord,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support lifecycle records")
    }
    /// Atomically commit the record and all sourced messages at their canonical positions.
    async fn commit_boundary(
        &self,
        _commit: &crate::extensions::BoundaryCommit,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support lifecycle boundary commits")
    }
    async fn record_request_view(
        &self,
        _record: &crate::extensions::RequestViewRecord,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support request view records")
    }
    async fn record_interception(
        &self,
        _record: &crate::extensions::InterceptionRecord,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support tool interception records")
    }
    async fn record_auxiliary(
        &self,
        _record: &crate::auxiliary::AuxiliaryRecord,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support auxiliary model records")
    }
    async fn composition_state(
        &self,
        _namespace: &str,
        _max_bytes: usize,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(None)
    }
    async fn commit_composition_state(
        &self,
        _namespace: &str,
        _state: &serde_json::Value,
    ) -> anyhow::Result<()> {
        anyhow::bail!("journal does not support branch-local composition state")
    }
    /// Called before terminal run events. Must retain the stop even when execution was cancelled.
    async fn finish(&self, outcome: &RunOutcome) -> anyhow::Result<()>;
}
#[async_trait]
impl RunJournal for () {
    async fn record_message_finalization(
        &self,
        _: &crate::extensions::MessageFinalizationRecord,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn finalize_message(
        &self,
        _: &crate::extensions::MessageFinalizationCommit,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn record_finalization(
        &self,
        _: &crate::extensions::FinalizationRecord,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn finalize_assistant(
        &self,
        _: usize,
        _: &crate::extensions::FinalizationRecord,
        _: &Message,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    fn supports_custom_input(&self) -> bool {
        true
    }
    async fn input_custom(&self, _: usize, _: &crate::RunInput) -> anyhow::Result<()> {
        Ok(())
    }
    async fn record_input_hooks(
        &self,
        _: &crate::extensions::InputHookRecord,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn input_prepared(
        &self,
        _: usize,
        _: &crate::RunInput,
        _: &crate::extensions::InputReceipt,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn record_lifecycle(&self, _: &crate::extensions::LifecycleRecord) -> anyhow::Result<()> {
        Ok(())
    }
    async fn commit_boundary(&self, _: &crate::extensions::BoundaryCommit) -> anyhow::Result<()> {
        Ok(())
    }

    async fn record_request_view(
        &self,
        _: &crate::extensions::RequestViewRecord,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn record_interception(
        &self,
        _: &crate::extensions::InterceptionRecord,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn record_auxiliary(&self, _: &crate::auxiliary::AuxiliaryRecord) -> anyhow::Result<()> {
        Ok(())
    }
    async fn checkpoint(&self, _: usize, _: &Message) -> anyhow::Result<()> {
        Ok(())
    }
    async fn record_composition(
        &self,
        _: &crate::composition::CompositionRecord,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn commit_composition_state(&self, _: &str, _: &serde_json::Value) -> anyhow::Result<()> {
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> anyhow::Result<()> {
        Ok(())
    }
}
