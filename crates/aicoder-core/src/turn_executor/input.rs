use super::*;
use crate::extensions::*;
impl TurnExecutor {
    pub(crate) fn input_pipeline(&self) -> Option<Arc<InputPipeline>> {
        if self.input_hooks.is_empty() && self.input_expander.is_none() {
            return None;
        }
        Some(Arc::new(InputPipeline {
            owner: self.request_owner,
            workspace: self.workspace_root().to_owned(),
            hooks: self.input_hooks.clone(),
            expander: self.input_expander.clone(),
            budget: self.config.budget.clone(),
        }))
    }
    /// Freeze the run's input processor before exposing its handle to producers.
    pub fn control_channel(
        &self,
        parent: TurnExecutionContext,
        config: crate::RunControlConfig,
    ) -> Result<(crate::RunHandle, crate::RunControl), crate::RunControlError> {
        let (handle, control) = crate::RunHandle::channel(parent, config)?;
        control.shared.bind_input_pipeline(self.input_pipeline());
        Ok((handle, control))
    }
    pub async fn prepare_user_input(
        &self,
        input: crate::AgentInput,
        delivery: InputDelivery,
        context: TurnExecutionContext,
    ) -> Result<InputPreparation> {
        self.prepare_user_input_at(input, delivery, &context, tokio::time::Instant::now())
            .await
    }
    pub(crate) async fn prepare_user_input_at(
        &self,
        input: crate::AgentInput,
        delivery: InputDelivery,
        context: &TurnExecutionContext,
        started: tokio::time::Instant,
    ) -> Result<InputPreparation> {
        if let Some(control) = &context.control {
            control.check_input_pipeline()?;
        }
        match self.input_pipeline() {
            Some(pipeline) => pipeline.prepare(input, delivery, context, started).await,
            None => {
                anyhow::ensure!(
                    input.prepared.is_none(),
                    "input snapshot belongs to another executor"
                );
                if context.is_cancelled() {
                    return Err(context.error().into());
                }
                Ok(InputPreparation::Ready(input))
            }
        }
    }
    pub(super) async fn admit_input_preparation(
        &self,
        prepared: Option<PreparedUserInput>,
        committed: bool,
        context: &TurnExecutionContext,
        journal: &dyn crate::RunJournal,
        limits: &Arc<RunLimiter>,
    ) -> Result<()> {
        if let Some(control) = &context.control {
            control.check_input_pipeline()?;
        }
        if !self.input_hooks.is_empty() {
            let record = InputHookRecord::Started {
                hooks: self
                    .input_hooks
                    .iter()
                    .map(|h| format!("{}/{}", h.owner, h.spec.id))
                    .collect(),
            };
            tokio::select! {biased;cancelled=context.cancelled()=>return Err(cancelled.into()),result=tokio::time::timeout(std::time::Duration::from_secs(30),journal.record_input_hooks(&record))=>result.map_err(|_|anyhow::anyhow!("input hook journal timed out")).and_then(|v|v).map_err(|e|RunFailure::from_error(RunStage::Persistence,&e))?,}
        }
        if let Some(prepared) = prepared {
            anyhow::ensure!(
                prepared.owner == self.request_owner,
                "input snapshot belongs to another executor"
            );
            limits.charge_output_bytes(prepared.receipt_bytes)?;
            if !committed && let Some(receipt) = prepared.receipt {
                let record = InputHookRecord::Prepared {
                    receipt: receipt.for_record(),
                };
                tokio::select! {biased;cancelled=context.cancelled()=>return Err(cancelled.into()),result=tokio::time::timeout(std::time::Duration::from_secs(30),journal.record_input_hooks(&record))=>result.map_err(|_|anyhow::anyhow!("input receipt timed out")).and_then(|v|v).map_err(|e|RunFailure::from_error(RunStage::Persistence,&e))?,}
            }
        }
        limits.check()
    }
}
