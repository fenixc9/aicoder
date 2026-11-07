use super::*;
use crate::{
    RunBudget, RunFailure, RunJournal, RunStop,
    extensions::*,
    model::*,
    run::{BudgetExceeded, BudgetResource, RunStage},
    run::{RunLimiter, RunProgress},
    session::*,
};
use std::{path::Path, sync::Arc, time::Instant};
pub const MANUAL_USAGE_NAMESPACE: &str = "aicoder.context.summary_usage.v1";
/// Accounting-only record for a manual summary that did not commit a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualSummaryUsage {
    pub version: u32,
    pub usage: ModelUsage,
    pub tokens: TokenAccounting,
    pub attempts: usize,
    pub stop: Option<RunStop>,
}
impl ManualSummaryUsage {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && (1..=32).contains(&self.attempts),
            "invalid manual summary accounting record"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualCompactionReport {
    pub session: Option<SessionMetadata>,
    pub checkpoint_entry_id: Option<String>,
    pub usage: ModelUsage,
    pub branch_usage: Option<ModelUsage>,
    pub tokens: TokenAccounting,
    pub attempts: usize,
    pub before: usize,
    pub after: usize,
    pub stop: Option<RunStop>,
    pub persistence_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auxiliary: Vec<crate::auxiliary::AuxiliaryAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_usage: Option<ModelUsage>,
    #[serde(default)]
    pub delivery: CompactionDelivery,
}
impl ManualCompactionReport {
    fn new() -> Self {
        Self {
            session: None,
            checkpoint_entry_id: None,
            branch_usage: None,
            usage: ModelUsage::zero(),
            tokens: TokenAccounting::default(),
            attempts: 0,
            before: 0,
            after: 0,
            stop: None,
            persistence_error: None,
            auxiliary: Vec::new(),
            primary_usage: None,
            delivery: Default::default(),
        }
    }
    pub fn succeeded(&self) -> bool {
        self.stop.is_none() && self.persistence_error.is_none()
    }
}
pub(crate) struct ManualHost<'a> {
    pub compactor: Option<Arc<dyn crate::ContextCompactor>>,
    pub hooks: Vec<crate::extensions::hooks::Registered<dyn CompactionHook>>,
    pub provider: Arc<dyn crate::ModelProvider>,
    pub input_context: Option<Arc<dyn crate::InputContextProvider>>,
    pub workspace: &'a Path,
    pub budget: RunBudget,
    pub measure: crate::context::ContextMeasure,
    pub policy: Arc<dyn crate::context_policy::ContextPolicy>,
}
pub(crate) async fn compact(
    host: ManualHost<'_>,
    repository: &dyn SessionRepository,
    id: &str,
    template: ModelRequest,
    context: TurnExecutionContext,
) -> ManualCompactionReport {
    let mut report = ManualCompactionReport::new();
    let context = context.child();
    let started = Instant::now();
    let limits = RunLimiter::new(host.budget.clone(), context.clone());
    let setup = (|| -> Result<_> {
        ensure!(
            !context.is_cancelled(),
            "manual summary cancelled before preparation"
        );
        let compactor = host
            .compactor
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("configure semantic context and a known window first"))?
            .clone();
        ensure!(
            compactor.persistent_summaries(),
            "manual summary requires semantic context strategy"
        );
        let mut session = repository.open_run(id)?;
        report.session = Some(session.metadata().clone());
        ensure!(
            session.metadata().cwd == host.workspace,
            "summary session belongs to another workspace"
        );
        let lease = session.acquire(repository)?;
        let input = crate::ExecutionInput::from_run_session(template, &session)?;
        Ok((session, lease, input, compactor))
    })();
    let (session, _lease, mut input, compactor) = match setup {
        Ok(value) => value,
        Err(error) => {
            report.stop = Some(if context.is_cancelled() {
                RunStop::Cancelled {
                    reason: context.reason().to_string(),
                }
            } else {
                RunStop::Failed(RunFailure::from_error(RunStage::Compaction, &error))
            });
            return report;
        }
    };
    let history_ids = session
        .messages()
        .iter()
        .map(|m| m.id.to_owned())
        .collect::<Vec<_>>();
    let journal = crate::session::SessionJournal::idle(repository, session);
    let source_projection = journal.boundary_context().await;
    let mut hook_measure = host.measure.clone();
    let result = {
        let work = async {
            if context.is_cancelled() {
                return Err(context.error().into());
            }
            if let Some(provider) = host.input_context {
                let prepared = tokio::select! {biased;cancelled=context.cancelled()=>return Err(anyhow::Error::from(cancelled)),prepared=provider.prepare_for_execution(host.workspace,&input.history,input.system_prefix_len,&context)=>prepared?};
                crate::input_context::validate_prefix(&prepared.system_prefix)?;
                let history = input.history.split_off(input.system_prefix_len);
                let messages = input.request.messages.split_off(input.system_prefix_len);
                input.system_prefix_len = prepared.system_prefix.len();
                input.history = prepared.system_prefix.clone();
                input.history.extend(history);
                input.request.messages = prepared.system_prefix;
                input.request.messages.extend(messages);
            }
            let measure = &mut hook_measure;
            let count = measure.count(&input.request.messages, Some(&input.request.tools));
            let decision = crate::context_policy::decide(
                host.policy.as_ref(),
                &crate::context_policy::ContextPolicyInput {
                    spec: &measure.spec,
                    messages: &input.request.messages,
                    tools: &input.request.tools,
                    options: &input.request.options,
                    tool_choice: &input.request.tool_choice,
                    count: &count,
                    compactor_target: Some(measure.target_tokens as u64),
                    legacy_compactor_estimate: None,
                    output_reservation: u64::from(
                        input
                            .request
                            .options
                            .max_output_tokens
                            .or(measure.spec.max_output_tokens)
                            .unwrap_or(0),
                    ),
                },
            )?;
            match decision {
                crate::context_policy::ContextDecision::Compact { target_tokens } => {
                    measure.target_tokens = target_tokens.min(usize::MAX as u64) as usize
                }
                crate::context_policy::ContextDecision::Reject { code } => {
                    anyhow::bail!("context policy rejected manual summary: {code}")
                }
                crate::context_policy::ContextDecision::Keep => {}
            }
            let projection = source_projection?;
            let projection = projection
                .as_ref()
                .map(|projection| {
                    CompactionProjection::from_session_view(
                        projection,
                        &input.request.messages,
                        input.system_prefix_len,
                        &input.history,
                        history_ids
                            .iter()
                            .enumerate()
                            .map(|(i, id)| (id.as_str(), input.system_prefix_len + i)),
                    )
                })
                .transpose()?
                .flatten();
            if host
                .budget
                .max_duration
                .is_some_and(|d| started.elapsed() >= d)
            {
                context.cancel("manual summary duration budget reached");
                return Err(context.error().into());
            }
            let request = CompactionRequest {
                input: ContextCompactionInput {
                    model: &input.request.model,
                    messages: &input.request.messages,
                    tools: Some(&input.request.tools),
                    max_output_tokens: input.request.options.max_output_tokens,
                },
                history: &input.history,
                system_prefix_len: input.system_prefix_len,
                force: true,
                token_budget: limits.remaining_tokens(),
                archived_messages: input.source.as_ref().map_or(0, |s| s.archived_messages),
            };
            let scope = super::hooks::HookScope {
                hooks: &host.hooks,
                workspace: host.workspace,
                journal: &journal,
                limits: &limits,
                context: &context,
                trigger: CompactionTrigger::Manual,
                request,
                template: &input.request,
                measure,
                provider: host.provider.as_ref(),
                round: 0,
                base_index: input.history.len(),
            };
            super::hooks::execute(&scope, compactor.as_ref(), projection.as_ref()).await
        };
        tokio::pin!(work);
        if let Some(duration) = host.budget.max_duration {
            tokio::select! {biased;_ = tokio::time::sleep(duration.saturating_sub(started.elapsed()))=>{context.cancel("manual summary duration budget reached");work.await},result=&mut work=>result}
        } else {
            work.await
        }
    };
    let mut checkpoint = None;
    let mut native = None;
    let mut from_extension = false;
    let mut service_usage = ModelUsage::zero();
    let mut service_tokens = TokenAccounting::default();
    let mut service_attempts = 0;
    let mut failed_event = None;
    match result {
        Ok(super::hooks::CompactionRun {
            recorded,
            native: prepared,
            from_extension: extension,
        }) => {
            service_usage = recorded.compaction.usage;
            service_tokens = recorded.accounting.unwrap_or_default();
            service_attempts = recorded.attempts;
            report.before = recorded.compaction.estimated_tokens_before;
            report.after = recorded.compaction.estimated_tokens_after;
            checkpoint = recorded.checkpoint;
            native = prepared;
            from_extension = extension;
        }
        Err(error) => {
            failed_event = Some(super::hooks::failure_event(
                CompactionTrigger::Manual,
                &error,
                &context,
            ));
            if let Some(failure) = error.downcast_ref::<SummaryFailure>() {
                service_usage = failure.usage.clone();
                service_tokens = failure.tokens.clone();
                service_attempts = failure.attempts;
                report.stop = Some(match failure.kind {
                    SummaryFailureKind::Cancelled => RunStop::Cancelled {
                        reason: context.reason().to_string(),
                    },
                    SummaryFailureKind::Vetoed => RunStop::Cancelled {
                        reason: failure.message.clone(),
                    },
                    SummaryFailureKind::Budget => RunStop::Budget(BudgetExceeded {
                        resource: BudgetResource::Tokens,
                        limit: host.budget.max_tokens.unwrap_or(0),
                        used: service_tokens.used,
                        requested: None,
                    }),
                    _ => RunStop::Failed(RunFailure::from_error(RunStage::Compaction, &error)),
                });
            } else {
                report.stop = Some(RunStop::Failed(RunFailure::from_error(
                    RunStage::Compaction,
                    &error,
                )));
            }
        }
    }
    limits.account_compaction(&service_tokens);
    let mut measured = RunProgress::new(Vec::new());
    measured.usage = service_usage.clone();
    limits.snapshot(&mut measured);
    report.usage = measured.usage;
    report.primary_usage = measured.primary_usage;
    report.auxiliary = measured.auxiliary;
    report.tokens = measured.tokens;
    report.attempts = service_attempts;
    if let Err(error) = journal.settle_idle_auxiliary(&report.auxiliary) {
        report.persistence_error = Some(crate::redaction::sanitize_text(&error.to_string(), 2048));
        checkpoint = None;
        native = None;
    }
    if let Err(error) = limits.check()
        && report.stop.is_none()
    {
        checkpoint = None;
        native = None;
        report.stop = Some(
            if let Some(budget) = error.downcast_ref::<BudgetExceeded>() {
                RunStop::Budget(budget.clone())
            } else {
                RunStop::Failed(RunFailure::from_error(RunStage::Compaction, &error))
            },
        );
    }
    if context.is_cancelled()
        || host
            .budget
            .max_duration
            .is_some_and(|d| started.elapsed() >= d)
    {
        checkpoint = None;
        native = None;
        report.stop = Some(
            if host
                .budget
                .max_duration
                .is_some_and(|d| started.elapsed() >= d)
            {
                RunStop::Budget(BudgetExceeded {
                    resource: BudgetResource::Duration,
                    limit: host.budget.max_duration.map_or(0, crate::run::millis),
                    used: crate::run::millis(started.elapsed()),
                    requested: None,
                })
            } else {
                RunStop::Cancelled {
                    reason: context.reason().to_string(),
                }
            },
        );
    }
    if let Some(prepared) = native {
        let id = prepared
            .session_projection()
            .and_then(|p| p.source.summary_checkpoint.clone());
        match journal.commit_prepared_boundary(prepared).await {
            Ok(()) => report.checkpoint_entry_id = id,
            Err(error) => {
                report.persistence_error =
                    Some(crate::redaction::sanitize_text(&error.to_string(), 2048))
            }
        }
    } else if let Some(draft) = checkpoint {
        match journal
            .record_summary(&draft, input.history.len(), input.history.len())
            .await
        {
            Ok(id) => report.checkpoint_entry_id = id,
            Err(error) => {
                report.persistence_error =
                    Some(crate::redaction::sanitize_text(&error.to_string(), 2048))
            }
        }
    } else if service_attempts > 0 {
        let data = serde_json::to_value(ManualSummaryUsage {
            version: 1,
            usage: service_usage,
            tokens: service_tokens,
            attempts: service_attempts,
            stop: report.stop.clone(),
        })
        .expect("manual usage");
        if let Err(error) = journal.append_idle(vec![SessionEntryData::Extension {
            namespace: MANUAL_USAGE_NAMESPACE.into(),
            data,
        }]) {
            report.persistence_error =
                Some(crate::redaction::sanitize_text(&error.to_string(), 2048));
        }
    }
    let event = failed_event.unwrap_or_else(|| CompactionEvent {
        trigger: CompactionTrigger::Manual,
        status: if report.stop.is_some() {
            CompactionStatus::Cancelled
        } else if report.persistence_error.is_some() {
            CompactionStatus::Failed
        } else if report.checkpoint_entry_id.is_some() {
            CompactionStatus::Completed
        } else {
            CompactionStatus::Unchanged
        },
        from_extension,
        checkpoint_entry_id: report.checkpoint_entry_id.clone(),
        before: report.before,
        after: report.after,
    });
    report.delivery = super::hooks::observe(&host.hooks, &journal, &limits, &context, event).await;
    if let Err(error) = limits.check()
        && report.stop.is_none()
    {
        report.stop = Some(
            if let Some(budget) = error.downcast_ref::<BudgetExceeded>() {
                RunStop::Budget(budget.clone())
            } else {
                RunStop::Failed(RunFailure::from_error(RunStage::Compaction, &error))
            },
        );
    }
    report.branch_usage = journal.branch_accounting().ok().map(|a| a.usage);
    report.session = Some(journal.metadata());
    report
}
