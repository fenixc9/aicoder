//! One shared native interception phase for automatic and manual compaction.
use super::*;
use crate::{
    RunJournal,
    context::ContextMeasure,
    extensions::hooks::{Registered, invoke},
    extensions::*,
    run::RunLimiter,
};
use std::{path::Path, time::Duration};
pub const COMPACTION_HOOK_NAMESPACE: &str = "aicoder.compaction-hooks.v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionChange {
    pub extension: String,
    pub hook: String,
    pub cancelled: bool,
    pub replaced: bool,
    pub defaulted: bool,
    pub instruction_bytes: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompactionHookRecord {
    Started {
        hooks: Vec<String>,
    },
    Prepared {
        trigger: CompactionTrigger,
        changes: Vec<CompactionChange>,
    },
    Observed {
        event: CompactionEvent,
        delivery: CompactionDelivery,
    },
}
pub(crate) struct HookScope<'a> {
    pub hooks: &'a [Registered<dyn CompactionHook>],
    pub workspace: &'a Path,
    pub journal: &'a dyn RunJournal,
    pub limits: &'a RunLimiter,
    pub context: &'a TurnExecutionContext,
    pub trigger: CompactionTrigger,
    pub request: CompactionRequest<'a>,
    pub template: &'a crate::model::ModelRequest,
    pub measure: &'a ContextMeasure,
    pub provider: &'a dyn crate::ModelProvider,
    pub round: usize,
    pub base_index: usize,
}
pub(crate) struct HookPreparation {
    pub instructions: Option<SummaryInstructions>,
    pub replacement: Option<PreparedBoundary>,
}
pub(crate) async fn probe(
    hooks: &[Registered<dyn CompactionHook>],
    journal: &dyn RunJournal,
    context: &TurnExecutionContext,
) -> Result<()> {
    if hooks.is_empty() {
        return Ok(());
    }
    let record = CompactionHookRecord::Started {
        hooks: hooks
            .iter()
            .map(|h| format!("{}/{}", h.owner, h.spec.id))
            .collect(),
    };
    tokio::select! {biased;cancelled=context.cancelled()=>Err(cancelled.into()),result=tokio::time::timeout(Duration::from_secs(30),journal.record_compaction_hooks(&record))=>result.map_err(|_|anyhow::anyhow!("compaction hook registration timed out"))?}
}
fn local_failure(context: &TurnExecutionContext, veto: bool) -> SummaryFailure {
    SummaryFailure {
        kind: if veto {
            SummaryFailureKind::Vetoed
        } else if context.is_cancelled() {
            SummaryFailureKind::Cancelled
        } else {
            SummaryFailureKind::InvalidResponse
        },
        message: if veto {
            "compaction vetoed by extension"
        } else {
            "compaction hook preparation failed"
        }
        .into(),
        usage: ModelUsage::zero(),
        tokens: Default::default(),
        attempts: 0,
    }
}
pub(crate) async fn before(scope: &HookScope<'_>) -> Result<HookPreparation> {
    if scope.hooks.is_empty() {
        return Ok(HookPreparation {
            instructions: None,
            replacement: None,
        });
    }
    let mut from_extension = false;
    let work = async {
        scope.limits.check()?;
        probe(scope.hooks, scope.journal, scope.context).await?;
        let session = scope.journal.boundary_context().await?;
        let mut instructions = None;
        let mut replacement: Option<(CompactionReplacement, BoundaryProposal)> = None;
        let mut changes = Vec::new();
        let mut bytes = 0usize;
        for h in scope.hooks {
            scope.limits.check()?;
            let session_ref = session.as_ref();
            let old_instructions = instructions.as_ref();
            let old_replacement = replacement.as_ref().map(|(r, _)| r);
            let action = invoke(&h.owner, &h.spec, scope.context.ui(), |cancel| async move {
                let models = BoundaryModels {
                    owner: &h.owner,
                    hook: &h.spec.id,
                    phase: "compaction",
                    limits: scope.limits,
                    journal: scope.journal,
                    counting: &scope.measure.counting,
                    execution: scope.context,
                    cancellation: &cancel,
                };
                h.callback
                    .before(
                        CompactionHookInput {
                            workspace: scope.workspace,
                            trigger: scope.trigger,
                            context: scope.request.input.messages,
                            history: scope.request.history,
                            system_prefix_len: scope.request.system_prefix_len,
                            session_context: session_ref,
                            target_tokens: scope.measure.target_tokens,
                            instructions: old_instructions,
                            replacement: old_replacement,
                            models: &models,
                        },
                        cancel.clone(),
                    )
                    .await
            })
            .await?;
            if let Some(action) = action {
                ensure!(
                    !(action.use_default && action.replacement.is_some()),
                    "conflicting compaction hook action"
                );
                let instruction_bytes = action.instructions.as_ref().map(|v| v.text.len());
                changes.push(CompactionChange {
                    extension: h.owner.clone(),
                    hook: h.spec.id.clone(),
                    cancelled: action.cancel,
                    replaced: action.replacement.is_some(),
                    defaulted: action.use_default,
                    instruction_bytes,
                });
                if let Some(value) = action.instructions {
                    value.validate()?;
                    instructions = Some(value);
                }
                if action.use_default {
                    replacement = None;
                    from_extension = false;
                }
                if let Some(value) = action.replacement {
                    from_extension = true;
                    let entry = value.entry();
                    entry.validate()?;
                    replacement = Some((
                        value,
                        BoundaryProposal::staged(h.owner.clone(), h.spec.id.clone(), entry),
                    ));
                }
                bytes = bytes.saturating_add(size(
                    &(&instructions, replacement.as_ref().map(|(v, _)| v)),
                    64 * 1024,
                )?);
                ensure!(
                    bytes <= 256 * 1024,
                    "compaction hook chain data budget exceeded"
                );
                if action.cancel {
                    scope
                        .journal
                        .record_compaction_hooks(&CompactionHookRecord::Prepared {
                            trigger: scope.trigger,
                            changes,
                        })
                        .await?;
                    return Err(anyhow::Error::new(local_failure(scope.context, true)));
                }
            }
        }
        scope.limits.check()?;
        scope
            .journal
            .record_compaction_hooks(&CompactionHookRecord::Prepared {
                trigger: scope.trigger,
                changes,
            })
            .await?;
        let replacement = if let Some((_, proposal)) = replacement {
            let commit = BoundaryCommit {
                message_finalizations: vec![],
                startup: None,
                kind: BoundaryKind::Compaction,
                round: scope.round,
                base_index: scope.base_index,
                entries: vec![proposal],
                continue_run: false,
                changes: vec![],
            };
            Some(
                scope
                    .journal
                    .prepare_boundary(
                        &commit,
                        scope.request.input.messages,
                        scope.request.system_prefix_len,
                    )
                    .await?,
            )
        } else {
            None
        };
        scope.limits.check()?;
        Ok::<_, anyhow::Error>(HookPreparation {
            instructions,
            replacement,
        })
    };
    let result = tokio::select! {biased;cancelled=scope.context.cancelled()=>Err(anyhow::Error::from(cancelled)),result=tokio::time::timeout(Duration::from_secs(30),work)=>result.map_err(|_|anyhow::anyhow!("compaction hook phase timed out")).and_then(|v|v)};
    result.map_err(|e| {
        let error = if e.downcast_ref::<SummaryFailure>().is_some() {
            e
        } else {
            local_failure(scope.context, false).into()
        };
        error.context(CompactionFailureProvenance { from_extension })
    })
}
pub(crate) fn replacement(
    scope: &HookScope<'_>,
    prepared: &PreparedBoundary,
) -> Result<RecordedCompaction> {
    let messages = prepared
        .projection()
        .ok_or_else(|| anyhow::anyhow!("compaction replacement has no prepared model view"))?
        .to_vec();
    ensure!(
        messages.get(..scope.request.system_prefix_len)
            == scope
                .request
                .input
                .messages
                .get(..scope.request.system_prefix_len),
        "compaction changed runtime prefix"
    );
    let mut check = scope.template.clone();
    check.messages = crate::tokens::model_messages(&messages, &scope.measure.spec).into_owned();
    scope.provider.validate_request(&check)?;
    let before = scope
        .measure
        .count(scope.request.input.messages, scope.request.input.tools)
        .tokens
        .min(usize::MAX as u64) as usize;
    let after = scope
        .measure
        .count(&messages, scope.request.input.tools)
        .tokens
        .min(usize::MAX as u64) as usize;
    ensure!(
        after <= scope.measure.target_tokens && after < before,
        "native compaction did not reduce context within target"
    );
    Ok(RecordedCompaction {
        compaction: crate::context::ContextCompaction {
            estimated_tokens_before: before,
            estimated_tokens_after: after,
            removed_messages: scope
                .request
                .input
                .messages
                .len()
                .saturating_sub(messages.len()),
            messages,
            usage: ModelUsage::zero(),
        },
        checkpoint: None,
        accounting: Some(Default::default()),
        diagnostics: vec![],
        attempts: 0,
    })
}
pub(crate) async fn observe(
    hooks: &[Registered<dyn CompactionHook>],
    journal: &dyn RunJournal,
    limits: &RunLimiter,
    context: &TurnExecutionContext,
    event: CompactionEvent,
) -> CompactionDelivery {
    if hooks.is_empty() {
        return CompactionDelivery::default();
    }
    let mut delivery = CompactionDelivery {
        registered: hooks.len(),
        ..Default::default()
    };
    let phase = tokio::time::Instant::now() + Duration::from_secs(30);
    let deadline = limits
        .budget
        .max_duration
        .and_then(|d| limits.started.checked_add(d))
        .map_or(phase, |d| d.min(phase));
    for h in hooks {
        if context.is_cancelled() || tokio::time::Instant::now() >= deadline {
            delivery.failures.push(CompactionObservationFailure {
                extension: h.owner.clone(),
                hook: h.spec.id.clone(),
                class: "scope_expired".into(),
            });
            continue;
        }
        delivery.invoked += 1;
        let result = tokio::select! {biased;_=context.cancelled()=>Err(anyhow::anyhow!("expired")),result=tokio::time::timeout_at(deadline,invoke(&h.owner,&h.spec,context.ui(),|c|h.callback.after(&event,c)))=>result.map_err(anyhow::Error::from).and_then(|v|v)};
        if result.is_ok() {
            delivery.completed += 1;
        } else {
            delivery.failures.push(CompactionObservationFailure {
                extension: h.owner.clone(),
                hook: h.spec.id.clone(),
                class: "callback_failed".into(),
            });
        }
    }
    let record = CompactionHookRecord::Observed {
        event,
        delivery: delivery.clone(),
    };
    if !hooks.is_empty() {
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            journal.record_compaction_hooks(&record),
        )
        .await;
        if !matches!(result, Ok(Ok(()))) {
            limits.fail_persistence(&anyhow::anyhow!("compaction observation receipt failed"));
        }
    }
    delivery
}
#[derive(Debug)]
pub(crate) struct CompactionFailureProvenance {
    pub from_extension: bool,
}
impl std::fmt::Display for CompactionFailureProvenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native compaction preparation failed")
    }
}
impl std::error::Error for CompactionFailureProvenance {}
pub(crate) struct CompactionRun {
    pub recorded: RecordedCompaction,
    pub native: Option<PreparedBoundary>,
    pub from_extension: bool,
}
pub(crate) async fn execute(
    scope: &HookScope<'_>,
    compactor: &dyn crate::ContextCompactor,
    projection: Option<&CompactionProjection>,
) -> Result<CompactionRun> {
    if scope.hooks.is_empty() {
        return Ok(CompactionRun {
            recorded: compactor
                .compact_projected(
                    scope.request,
                    scope.measure.clone(),
                    projection,
                    scope.context.clone(),
                )
                .await?,
            native: None,
            from_extension: false,
        });
    }
    let prepared = before(scope).await?;
    if let Some(native) = prepared.replacement {
        let recorded = replacement(scope, &native).map_err(|_| {
            anyhow::Error::new(local_failure(scope.context, false)).context(
                CompactionFailureProvenance {
                    from_extension: true,
                },
            )
        })?;
        Ok(CompactionRun {
            recorded,
            native: Some(native),
            from_extension: true,
        })
    } else {
        let mut recorded = compactor
            .compact_customized(
                CompactionRequest {
                    token_budget: scope.limits.remaining_tokens(),
                    ..scope.request
                },
                scope.measure.clone(),
                projection,
                prepared.instructions.as_ref(),
                scope.context.clone(),
            )
            .await?;
        let valid = (|| -> Result<()> {
            let measured_before = scope
                .measure
                .count(scope.request.input.messages, scope.request.input.tools)
                .tokens
                .min(usize::MAX as u64) as usize;
            let measured_after = scope
                .measure
                .count(&recorded.compaction.messages, scope.request.input.tools)
                .tokens
                .min(usize::MAX as u64) as usize;
            let before = if compactor.supports_counting() {
                measured_before
            } else {
                recorded.compaction.estimated_tokens_before
            };
            let after = if compactor.supports_counting() {
                measured_after
            } else {
                recorded.compaction.estimated_tokens_after
            };
            let output = u64::from(
                scope
                    .template
                    .options
                    .max_output_tokens
                    .or(scope.measure.spec.max_output_tokens)
                    .unwrap_or(0),
            );
            ensure!(
                scope
                    .measure
                    .spec
                    .context_tokens
                    .is_none_or(|window| (measured_after as u64).saturating_add(output) <= window),
                "compactor exceeded known model window"
            );
            ensure!(
                recorded
                    .compaction
                    .messages
                    .get(..scope.request.system_prefix_len)
                    == scope
                        .request
                        .input
                        .messages
                        .get(..scope.request.system_prefix_len),
                "compactor changed runtime prefix"
            );
            if recorded.checkpoint.is_some()
                || recorded.compaction.messages != scope.request.input.messages
            {
                ensure!(
                    after <= scope.measure.target_tokens && after < before,
                    "compactor did not reduce context within target"
                );
            }
            let mut request = scope.template.clone();
            request.messages =
                crate::tokens::model_messages(&recorded.compaction.messages, &scope.measure.spec)
                    .into_owned();
            scope.provider.validate_request(&request)?;
            recorded.compaction.estimated_tokens_before = before;
            recorded.compaction.estimated_tokens_after = after;
            Ok(())
        })();
        if valid.is_err() {
            return Err(SummaryFailure {
                kind: SummaryFailureKind::InvalidResponse,
                message: "compaction output was not admitted".into(),
                usage: recorded.compaction.usage.clone(),
                tokens: recorded.accounting.clone().unwrap_or_else(|| {
                    TokenAccounting::from_usage(
                        &recorded.compaction.usage,
                        (recorded.compaction.estimated_tokens_before as u64)
                            .saturating_add(recorded.compaction.estimated_tokens_after as u64),
                    )
                }),
                attempts: recorded.attempts,
            }
            .into());
        }
        Ok(CompactionRun {
            recorded,
            native: None,
            from_extension: false,
        })
    }
}
pub(crate) fn failure_event(
    trigger: CompactionTrigger,
    error: &anyhow::Error,
    context: &TurnExecutionContext,
) -> CompactionEvent {
    let status = if error
        .downcast_ref::<SummaryFailure>()
        .is_some_and(|f| f.kind == SummaryFailureKind::Vetoed)
    {
        CompactionStatus::Vetoed
    } else if context.is_cancelled()
        || error
            .downcast_ref::<SummaryFailure>()
            .is_some_and(|f| f.kind == SummaryFailureKind::Cancelled)
    {
        CompactionStatus::Cancelled
    } else {
        CompactionStatus::Failed
    };
    CompactionEvent {
        trigger,
        status,
        from_extension: error
            .downcast_ref::<CompactionFailureProvenance>()
            .is_some_and(|v| v.from_extension),
        checkpoint_entry_id: None,
        before: 0,
        after: 0,
    }
}
