//! Raw input interception before expansion, persistence and steering notification.
use super::*;
use crate::{AgentInput, InputSource, TurnExecutionContext, model::Message};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
pub const INPUT_HOOK_NAMESPACE: &str = "aicoder.input-hooks.v1";
pub const MAX_INPUT_HOOK_BYTES: usize = 2 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputDelivery {
    Initial,
    Steer,
    FollowUp,
}
pub struct InputHookInput<'a> {
    pub workspace: &'a Path,
    pub input: &'a AgentInput,
    pub source: InputSource,
    pub delivery: InputDelivery,
}
#[derive(Debug)]
pub enum InputAction {
    Continue,
    Transform(AgentInput),
    Handled,
}
#[async_trait]
pub trait InputHook: Send + Sync {
    async fn input(
        &self,
        input: InputHookInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> anyhow::Result<InputAction>;
}
/// Host expansion runs after raw hooks. Preserve attachments and expand only the prompt block.
#[async_trait]
pub trait InputExpander: Send + Sync {
    async fn expand(&self, input: AgentInput) -> anyhow::Result<AgentInput>;
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputChange {
    pub extension: String,
    pub hook: String,
    pub transformed: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputReceipt {
    pub source: InputSource,
    pub delivery: InputDelivery,
    /// Ephemeral raw input for host failure recovery. Never serialized into history or traces.
    #[serde(skip)]
    pub original: Option<Message>,
    pub original_bytes: usize,
    pub effective_bytes: usize,
    pub changes: Vec<InputChange>,
    pub expanded: bool,
}
impl InputReceipt {
    pub(crate) fn for_record(&self) -> Self {
        Self {
            source: self.source,
            delivery: self.delivery,
            original: None,
            original_bytes: self.original_bytes,
            effective_bytes: self.effective_bytes,
            changes: self.changes.clone(),
            expanded: self.expanded,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputHookRecord {
    Started { hooks: Vec<String> },
    Prepared { receipt: InputReceipt },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputHandled {
    pub extension: String,
    pub hook: String,
    pub source: InputSource,
    pub delivery: InputDelivery,
}
impl fmt::Display for InputHandled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "input handled by {}/{}", self.extension, self.hook)
    }
}
impl std::error::Error for InputHandled {}
#[derive(Debug, Clone)]
pub enum InputPreparation {
    Ready(AgentInput),
    Handled(InputHandled),
}
#[derive(Debug, Clone)]
pub(crate) struct PreparedUserInput {
    pub owner: uuid::Uuid,
    pub run_id: Option<crate::RunId>,
    pub started: tokio::time::Instant,
    pub delivery: InputDelivery,
    pub receipt: Option<Arc<InputReceipt>>,
    pub receipt_bytes: usize,
}
#[derive(Clone)]
pub(crate) struct InputPipeline {
    pub owner: uuid::Uuid,
    pub workspace: PathBuf,
    pub hooks: Vec<Registered<dyn InputHook>>,
    pub expander: Option<Arc<dyn InputExpander>>,
    pub budget: crate::RunBudget,
}
impl InputPipeline {
    pub fn requires_async(&self) -> bool {
        !self.hooks.is_empty()
    }
    pub async fn prepare(
        &self,
        mut input: AgentInput,
        delivery: InputDelivery,
        context: &TurnExecutionContext,
        started: tokio::time::Instant,
    ) -> anyhow::Result<InputPreparation> {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let run_id = context.control.as_ref().map(|c| c.event_source().run_id());
        if let Some(prepared) = &input.prepared {
            anyhow::ensure!(
                prepared.owner == self.owner
                    && prepared.delivery == delivery
                    && prepared.run_id == run_id,
                "input snapshot belongs to another executor or delivery boundary"
            );
            if let Some(duration) = self.budget.max_duration
                && prepared.started.elapsed() >= duration
            {
                return Err(crate::BudgetExceeded {
                    resource: crate::BudgetResource::Duration,
                    limit: crate::run::millis(duration),
                    used: crate::run::millis(prepared.started.elapsed()),
                    requested: None,
                }
                .into());
            }
            return Ok(InputPreparation::Ready(input));
        }
        let source = input.source();
        let original = input.message().clone();
        let mut changes = Vec::new();
        let mut aggregate = 0usize;
        let original_bytes = size(&original, MAX_INPUT_HOOK_BYTES - 16384)?;
        let work = async {
            for hook in &self.hooks {
                let action = invoke(&hook.owner, &hook.spec, context.ui(), |cancellation| {
                    hook.callback.input(
                        InputHookInput {
                            workspace: &self.workspace,
                            input: &input,
                            source,
                            delivery,
                        },
                        cancellation,
                    )
                })
                .await?;
                match action {
                    InputAction::Continue => changes.push(InputChange {
                        extension: hook.owner.clone(),
                        hook: hook.spec.id.clone(),
                        transformed: false,
                    }),
                    InputAction::Handled => {
                        return Ok(InputPreparation::Handled(InputHandled {
                            extension: hook.owner.clone(),
                            hook: hook.spec.id.clone(),
                            source,
                            delivery,
                        }));
                    }
                    InputAction::Transform(next) => {
                        size(next.message(), MAX_INPUT_HOOK_BYTES - 16384)?;
                        let next = AgentInput::try_from(next.into_message())?.with_source(source);
                        aggregate = aggregate
                            .saturating_add(size(next.message(), MAX_INPUT_HOOK_BYTES - 16384)?);
                        anyhow::ensure!(
                            aggregate <= 8 * MAX_INPUT_HOOK_BYTES,
                            "input hook chain exceeds aggregate budget"
                        );
                        changes.push(InputChange {
                            extension: hook.owner.clone(),
                            hook: hook.spec.id.clone(),
                            transformed: next.message() != input.message(),
                        });
                        input = next;
                    }
                }
            }
            let mut expanded = false;
            if let Some(expander) = &self.expander {
                let next = expander.expand(input.clone()).await?;
                size(next.message(), MAX_INPUT_HOOK_BYTES - 16384)?;
                let next = AgentInput::try_from(next.into_message())?.with_source(source);
                expanded = next.message() != input.message();
                input = next;
            }
            let effective_bytes = size(input.message(), MAX_INPUT_HOOK_BYTES - 16384)?;
            let receipt = (!changes.is_empty() || expanded).then(|| {
                Arc::new(InputReceipt {
                    source,
                    delivery,
                    original_bytes,
                    effective_bytes,
                    original: (original != *input.message()).then_some(original),
                    changes,
                    expanded,
                })
            });
            let receipt_bytes = match &receipt {
                Some(r) => size(
                    &InputHookRecord::Prepared {
                        receipt: r.for_record(),
                    },
                    MAX_INPUT_HOOK_BYTES,
                )?,
                None => 0,
            };
            if let Some(limit) = self.budget.max_output_bytes {
                anyhow::ensure!(
                    receipt_bytes <= limit,
                    crate::BudgetExceeded {
                        resource: crate::BudgetResource::OutputBytes,
                        limit: limit as u64,
                        used: 0,
                        requested: Some(receipt_bytes as u64)
                    }
                );
            }
            input.prepared = Some(PreparedUserInput {
                owner: self.owner,
                run_id,
                started,
                delivery,
                receipt,
                receipt_bytes,
            });
            Ok(InputPreparation::Ready(input))
        };
        let deadline = self
            .budget
            .max_duration
            .map(|d| {
                started
                    .checked_add(d)
                    .ok_or_else(|| anyhow::anyhow!("run duration is too large"))
            })
            .transpose()?;
        let deadline_wait = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        let result = tokio::select! {biased;
            cancelled=context.cancelled()=>return Err(cancelled.into()),
            _=deadline_wait=>return Err(crate::BudgetExceeded{resource:crate::BudgetResource::Duration,limit:crate::run::millis(self.budget.max_duration.unwrap_or_default()),used:crate::run::millis(started.elapsed()),requested:None}.into()),
            result=tokio::time::timeout(Duration::from_secs(30),work)=>result.map_err(|_|anyhow::anyhow!("input hook phase timed out"))??,
        };
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
            return Err(crate::BudgetExceeded {
                resource: crate::BudgetResource::Duration,
                limit: crate::run::millis(self.budget.max_duration.unwrap_or_default()),
                used: crate::run::millis(started.elapsed()),
                requested: None,
            }
            .into());
        }
        Ok(result)
    }
}
