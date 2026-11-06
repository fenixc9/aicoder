use super::*;
use crate::extensions::*;
pub(super) struct FinalizeScope<'a> {
    pub original: Message,
    pub stop: &'a StopReason,
    pub usage: &'a crate::model::ModelUsage,
    pub round: usize,
    pub progress: &'a mut RunProgress,
    pub context: &'a TurnExecutionContext,
    pub limits: &'a Arc<RunLimiter>,
    pub journal: &'a dyn crate::RunJournal,
    pub events: &'a AgentEventEmitter,
}
impl TurnExecutor {
    pub(super) async fn start_finalization(
        &self,
        journal: &dyn crate::RunJournal,
        context: &TurnExecutionContext,
    ) -> Result<()> {
        if self.finalizers.is_empty() {
            return Ok(());
        }
        let record = FinalizationRecord::Started {
            hooks: self
                .finalizers
                .iter()
                .map(|h| format!("{}/{}", h.owner, h.spec.id))
                .collect(),
        };
        tokio::select! {biased;cancelled=context.cancelled()=>Err(cancelled.into()),result=tokio::time::timeout(std::time::Duration::from_secs(30),journal.record_finalization(&record))=>result.map_err(|_|anyhow::anyhow!("finalization journal timed out")).and_then(|v|v).map_err(|e|RunFailure::from_error(RunStage::Persistence,&e).into())}
    }
    pub(super) async fn finalize_assistant(&self, scope: FinalizeScope<'_>) -> Result<Message> {
        let FinalizeScope {
            original,
            stop,
            usage,
            round,
            progress,
            context,
            limits,
            journal,
            events,
        } = scope;
        if self.finalizers.is_empty() && self.message_finalizers.is_none() {
            return Ok(original);
        }
        limits.check()?;
        let original_limit = if self.finalizers.is_empty() {
            MAX_MESSAGE_FINALIZATION_BYTES
        } else {
            MAX_FINAL_MESSAGE_BYTES
        };
        let original_bytes = crate::extensions::size(&original, original_limit)?;
        let mut message = original.clone();
        let mut changes = Vec::new();
        let mut bytes = 0usize;
        let work = async {
            for hook in &self.finalizers {
                let next = invoke(&hook.owner, &hook.spec, context.ui(), |c| {
                    hook.callback.finalize(
                        AssistantMessageInput {
                            workspace: self.workspace_root(),
                            round,
                            original: &original,
                            message: &message,
                            usage,
                            stop_reason: stop,
                        },
                        c,
                    )
                })
                .await?;
                if let Some(next) = next {
                    validate_edit(&original, &next)?;
                    bytes = bytes
                        .saturating_add(crate::extensions::size(&next, MAX_FINAL_MESSAGE_BYTES)?);
                    anyhow::ensure!(
                        bytes <= 4 * MAX_FINAL_MESSAGE_BYTES,
                        "assistant finalization chain exceeds aggregate budget"
                    );
                    let changed = next != message;
                    message = next;
                    changes.push(FinalizationChange {
                        extension: hook.owner.clone(),
                        hook: hook.spec.id.clone(),
                        changed,
                    });
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::select! {biased;cancelled=context.cancelled()=>return Err(cancelled.into()),result=tokio::time::timeout(std::time::Duration::from_secs(30),work)=>result.map_err(|_|anyhow::anyhow!("assistant finalization phase timed out"))??}
        limits.check()?;
        let legacy = (!self.finalizers.is_empty()).then(|| FinalizationRecord::Applied {
            round,
            original_bytes,
            effective_bytes: crate::extensions::size(&message, MAX_FINAL_MESSAGE_BYTES)
                .expect("validated message"),
            changes,
        });
        let finalized = self
            .prepare_message_finalization(
                &message,
                None,
                MessageOrigin::Assistant,
                Some(round),
                context,
            )
            .await?;
        limits.check()?;
        if let Some(value) = &finalized {
            message = value.message.clone();
        }
        let index = progress.messages.len() - 1;
        if let Some(finalized) = &finalized {
            let commit = MessageFinalizationCommit {
                index,
                finalized: finalized.clone(),
                input: None,
                assistant: legacy,
            };
            limits.charge_output_bytes(crate::extensions::size(
                &commit,
                MAX_MESSAGE_FINALIZATION_BYTES + 128 * 1024,
            )?)?;
            tokio::select! {biased;cancelled=context.cancelled()=>return Err(cancelled.into()),result=tokio::time::timeout(std::time::Duration::from_secs(30),journal.finalize_message(&commit))=>result.map_err(|_|anyhow::anyhow!("assistant message finalization commit timed out")).and_then(|v|v).map_err(|e|RunFailure::from_error(RunStage::Persistence,&e))?}
        } else {
            let record = legacy.expect("installed assistant hooks");
            crate::extensions::size(&record, 64 * 1024)?;
            limits.charge_output_bytes(crate::extensions::size(
                &(&record, &message),
                MAX_FINAL_MESSAGE_BYTES + 64 * 1024,
            )?)?;
            tokio::select! {biased;cancelled=context.cancelled()=>return Err(cancelled.into()),result=tokio::time::timeout(std::time::Duration::from_secs(30),journal.finalize_assistant(index,&record,&message))=>result.map_err(|_|anyhow::anyhow!("assistant finalization commit timed out")).and_then(|v|v).map_err(|e|RunFailure::from_error(RunStage::Persistence,&e))?}
        }
        progress.messages[index] = message.clone();
        if let Some(value) = finalized {
            events.emit(value.event());
        }
        events.emit(AgentRawEvent::AssistantMessageFinalized {
            message: Arc::new(message.clone()),
            changed: message != original,
        });
        limits.check()?;
        Ok(message)
    }
}
