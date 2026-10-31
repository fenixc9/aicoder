use super::*;
use crate::{
    RunBudget, RunFailure, RunStop,
    model::*,
    run::{BudgetExceeded, BudgetResource, RunStage},
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
        }
    }
    pub fn succeeded(&self) -> bool {
        self.stop.is_none() && self.persistence_error.is_none()
    }
}
pub(crate) struct ManualHost<'a> {
    pub compactor: Option<Arc<dyn crate::ContextCompactor>>,
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
    let (mut session, _lease, mut input, compactor) = match setup {
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
    let work = async {
        if let Some(provider) = host.input_context {
            let prepared = tokio::select! {biased;cancelled=context.cancelled()=>return Err(anyhow::Error::from(cancelled)),prepared=provider.prepare_with_prefix(host.workspace,&input.history,input.system_prefix_len)=>prepared?};
            crate::input_context::validate_prefix(&prepared.system_prefix)?;
            let history = input.history.split_off(input.system_prefix_len);
            let messages = input.request.messages.split_off(input.system_prefix_len);
            input.system_prefix_len = prepared.system_prefix.len();
            input.history = prepared.system_prefix.clone();
            input.history.extend(history);
            input.request.messages = prepared.system_prefix;
            input.request.messages.extend(messages);
        }
        let mut measure = host.measure.clone();
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
        compactor
            .compact_counted(
                CompactionRequest {
                    input: ContextCompactionInput {
                        model: &input.request.model,
                        messages: &input.request.messages,
                        tools: Some(&input.request.tools),
                        max_output_tokens: input.request.options.max_output_tokens,
                    },
                    history: &input.history,
                    system_prefix_len: input.system_prefix_len,
                    force: true,
                    token_budget: host.budget.max_tokens,
                    archived_messages: input
                        .source
                        .as_ref()
                        .map_or(0, |source| source.archived_messages),
                },
                measure,
                context.clone(),
            )
            .await
    };
    tokio::pin!(work);
    let result = if let Some(duration) = host.budget.max_duration {
        tokio::select! {biased;_ = tokio::time::sleep(duration)=>{context.cancel("manual summary duration budget reached");work.await},result=&mut work=>result}
    } else {
        work.await
    };
    let mut checkpoint = None;
    match result {
        Ok(recorded) => {
            report.usage = recorded.compaction.usage;
            report.tokens = recorded.accounting.unwrap_or_default();
            report.attempts = recorded.attempts;
            report.before = recorded.compaction.estimated_tokens_before;
            report.after = recorded.compaction.estimated_tokens_after;
            checkpoint = recorded.checkpoint;
        }
        Err(error) => {
            if let Some(failure) = error.downcast_ref::<SummaryFailure>() {
                report.usage = failure.usage.clone();
                report.tokens = failure.tokens.clone();
                report.attempts = failure.attempts;
                report.stop = Some(match failure.kind {
                    SummaryFailureKind::Cancelled => RunStop::Cancelled {
                        reason: context.reason().to_string(),
                    },
                    SummaryFailureKind::Budget => RunStop::Budget(BudgetExceeded {
                        resource: BudgetResource::Tokens,
                        limit: host.budget.max_tokens.unwrap_or(0),
                        used: report.tokens.used,
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
    if context.is_cancelled() {
        checkpoint = None;
        report.stop = Some(
            if host
                .budget
                .max_duration
                .is_some_and(|duration| started.elapsed() >= duration)
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
    if let Some(draft) = checkpoint {
        let messages = session.messages();
        let Some(index) = messages.len().checked_sub(draft.keep_messages) else {
            report.persistence_error = Some("summary tail does not map to history".into());
            return report;
        };
        let Some(first) = messages.get(index).map(|message| message.id.to_string()) else {
            report.persistence_error = Some("summary tail is empty".into());
            return report;
        };
        let committed = match &mut session {
            RunSession::Full(session) => {
                super::session::commit_summary(repository, session, draft, first, None)
            }
            RunSession::Window(window) => {
                let checkpoint = super::SummaryCheckpoint {
                    version: 1,
                    source_leaf: window.metadata().leaf_id.clone().unwrap_or_default(),
                    source_revision: window.metadata().revision,
                    first_kept_entry_id: first,
                    summarized_messages: window
                        .source()
                        .recorded_messages
                        .saturating_sub(draft.keep_messages),
                    accounted_by_turn: None,
                    draft,
                };
                repository
                    .append_window(window, vec![checkpoint.data()])
                    .map(|()| window.metadata().leaf_id.clone().expect("summary entry"))
            }
        };
        match committed {
            Ok(id) => report.checkpoint_entry_id = Some(id),
            Err(error) => {
                report.persistence_error =
                    Some(crate::redaction::sanitize_text(&error.to_string(), 2048));
                report.session = Some(session.metadata().clone());
                return report;
            }
        }
    } else if report.attempts > 0 {
        let data = serde_json::to_value(ManualSummaryUsage {
            version: 1,
            usage: report.usage.clone(),
            tokens: report.tokens.clone(),
            attempts: report.attempts,
            stop: report.stop.clone(),
        })
        .expect("manual usage");
        if let Err(error) = session.append(
            repository,
            vec![SessionEntryData::Extension {
                namespace: MANUAL_USAGE_NAMESPACE.into(),
                data,
            }],
        ) {
            report.persistence_error =
                Some(crate::redaction::sanitize_text(&error.to_string(), 2048));
        }
    }
    report.branch_usage = session.accounting().ok().map(|accounting| accounting.usage);
    report.session = Some(session.metadata().clone());
    report
}
