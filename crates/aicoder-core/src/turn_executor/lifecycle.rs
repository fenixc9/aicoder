use super::*;
use crate::extensions::*;
use std::time::Duration;
pub(super) struct BoundaryScope<'a> {
    pub kind: BoundaryKind,
    pub prefix: usize,
    pub events: &'a AgentEventEmitter,
    pub request: &'a ModelRequest,
    pub context: &'a TurnExecutionContext,
    pub limits: &'a Arc<RunLimiter>,
    pub journal: &'a dyn crate::RunJournal,
}
impl TurnExecutor {
    pub(super) async fn start_lifecycle(
        &self,
        journal: &dyn crate::RunJournal,
        context: &TurnExecutionContext,
    ) -> Result<()> {
        if self.boundary_hooks.is_empty() && self.startup_hooks.is_empty() {
            return Ok(());
        }
        let record = LifecycleRecord::Started {
            hooks: self
                .boundary_hooks
                .iter()
                .map(|h| format!("{}/{}", h.owner, h.spec.id))
                .chain(
                    self.startup_hooks
                        .iter()
                        .map(|h| format!("{}/{}", h.owner, h.spec.id)),
                )
                .collect(),
        };
        tokio::select! {biased;cancelled=context.cancelled()=>Err(cancelled.into()),result=tokio::time::timeout(Duration::from_secs(30),journal.record_lifecycle(&record))=>result.map_err(|_|anyhow::anyhow!("lifecycle journal timed out")).and_then(|v|v).map_err(|e|RunFailure::from_error(RunStage::Persistence,&e).into())}
    }
    pub(super) async fn run_boundary(
        &self,
        scope: BoundaryScope<'_>,
        progress: &mut RunProgress,
        model_context: &mut Vec<Message>,
    ) -> Result<bool> {
        let BoundaryScope {
            kind,
            prefix,
            events,
            request,
            context,
            limits,
            journal,
        } = scope;
        limits.check()?;
        if self.boundary_hooks.is_empty() {
            return Ok(false);
        }
        let mut session_context = tokio::select! {biased;cancelled=context.cancelled()=>return Err(cancelled.into()),result=tokio::time::timeout(Duration::from_secs(30),journal.boundary_context())=>result.map_err(|_|anyhow::anyhow!("boundary context timed out"))??};
        limits.check()?;
        if context.control.as_ref().is_some_and(|c| c.has_steering()) {
            return Ok(false);
        }
        let mut preview_messages = model_context.clone();
        let mut entries = Vec::<BoundaryProposal>::new();
        let mut again = false;
        let mut changes = Vec::new();
        let mut bytes = 0usize;
        let work = async {
            for hook in &self.boundary_hooks {
                limits.check()?;
                let mut measured = RunProgress::new(Vec::new());
                measured.usage = progress.usage.clone();
                measured.primary_usage = progress.primary_usage.clone();
                limits.snapshot(&mut measured);
                let statistics = BoundaryStatistics {
                    rounds: progress.rounds,
                    tool_calls: measured.tool_calls,
                    usage: measured.usage,
                    primary_usage: measured.primary_usage,
                    auxiliary: measured.auxiliary,
                    tokens: measured.tokens,
                    output_bytes: measured.output_bytes,
                    elapsed: measured.elapsed,
                };
                let can_continue = progress.rounds < self.config.max_rounds
                    && limits.remaining_tokens() != Some(0);
                let history = progress.messages.as_slice();
                let staged = entries.as_slice();
                let session = session_context.as_ref();
                let preview = preview_messages.as_slice();
                let statistics = &statistics;
                let round = progress.rounds;
                let core_context = model_context.as_slice();
                let action = crate::extensions::invoke(
                    &hook.owner,
                    &hook.spec,
                    context.ui(),
                    |cancel| async move {
                        let models = BoundaryModels {
                            owner: &hook.owner,
                            hook: &hook.spec.id,
                            limits,
                            journal,
                            counting: &self.counting,
                            execution: context,
                            cancellation: &cancel,
                        };
                        hook.callback
                            .boundary(
                                BoundaryInput {
                                    workspace: self.workspace_root(),
                                    session_context: session,
                                    models: &models,
                                    context_preview: preview,
                                    kind,
                                    round,
                                    history,
                                    statistics,
                                    context: core_context,
                                    entries: staged,
                                    continue_requested: again,
                                    can_continue,
                                },
                                cancel.clone(),
                            )
                            .await
                    },
                )
                .await?;
                if let Some(action) = action {
                    let replaced = action.replace.is_some();
                    let appended = action.append.len();
                    let continuation = action.continue_run;
                    if let Some(replacement) = action.replace {
                        entries.clear();
                        for entry in replacement {
                            entry.validate()?;
                            entries.push(BoundaryProposal::staged(
                                hook.owner.clone(),
                                hook.spec.id.clone(),
                                entry,
                            ));
                        }
                    }
                    for entry in action.append {
                        entry.validate()?;
                        entries.push(BoundaryProposal::staged(
                            hook.owner.clone(),
                            hook.spec.id.clone(),
                            entry,
                        ));
                    }
                    anyhow::ensure!(entries.len() <= 32, "boundary entry count exceeds 32");
                    bytes = bytes
                        .saturating_add(crate::extensions::size(&entries, MAX_BOUNDARY_BYTES)?);
                    anyhow::ensure!(
                        bytes <= 4 * MAX_BOUNDARY_BYTES,
                        "boundary chain exceeds aggregate data budget"
                    );
                    if let Some(value) = continuation {
                        again = value;
                    }
                    changes.push(BoundaryChange {
                        extension: hook.owner.clone(),
                        hook: hook.spec.id.clone(),
                        replaced,
                        appended,
                        continue_run: continuation,
                    });
                    let preview = BoundaryCommit {
                        message_finalizations: Vec::new(),
                        startup: None,
                        kind,
                        round: progress.rounds,
                        base_index: progress.messages.len(),
                        entries: entries.clone(),
                        continue_run: again,
                        changes: changes.clone(),
                    };
                    let prepared = journal
                        .prepare_boundary(&preview, model_context, prefix)
                        .await?;
                    if let Some(view) = prepared.projection() {
                        preview_messages = view.to_vec();
                    }
                    if let Some(source) = prepared.session_projection() {
                        session_context = Some(source.clone());
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::select! {biased;
            cancelled=context.cancelled()=>return Err(cancelled.into()),
            _=async {match &context.control{Some(c)=>c.steering_requested().await,None=>std::future::pending().await}}=>return Ok(false),
            result=tokio::time::timeout(Duration::from_secs(30),work)=>result.map_err(|_|anyhow::anyhow!("lifecycle boundary phase timed out"))??
        }
        limits.check()?;
        if context.control.as_ref().is_some_and(|c| c.has_steering()) {
            return Ok(false);
        }
        if changes.is_empty() {
            return Ok(false);
        }
        let can_continue =
            progress.rounds < self.config.max_rounds && limits.remaining_tokens() != Some(0);
        if again && !can_continue {
            return Err(BudgetExceeded {
                resource: if progress.rounds >= self.config.max_rounds {
                    BudgetResource::Rounds
                } else {
                    BudgetResource::Tokens
                },
                limit: if progress.rounds >= self.config.max_rounds {
                    self.config.max_rounds as u64
                } else {
                    limits.budget.max_tokens.unwrap_or(0)
                },
                used: if progress.rounds >= self.config.max_rounds {
                    progress.rounds as u64
                } else {
                    limits
                        .budget
                        .max_tokens
                        .unwrap_or(0)
                        .saturating_sub(limits.remaining_tokens().unwrap_or(0))
                },
                requested: Some(1),
            }
            .into());
        }
        let mut commit = BoundaryCommit {
            message_finalizations: Vec::new(),
            startup: None,
            kind,
            round: progress.rounds,
            base_index: progress.messages.len(),
            entries,
            continue_run: again,
            changes,
        };
        self.finalize_context_commit(&mut commit, context).await?;
        limits.check()?;
        crate::extensions::size(&commit, 128 * 1024)?;
        let messages = commit.messages();
        if !messages.is_empty() {
            let mut check = ModelRequest::new(&request.model, messages.clone());
            check.options = request.options.clone();
            check.tools = request.tools.clone();
            check.tool_choice = request.tool_choice.clone();
            check.stream = request.stream;
            let spec = self.provider.model_spec(&check.model)?;
            check.messages = crate::tokens::model_messages(&check.messages, &spec).into_owned();
            self.provider.validate_request(&check)?;
        }
        let encoded = serde_json::to_string(&(&commit, &messages))?;
        let _ = limits.retain_text(&encoded);
        limits.check()?;
        if context.control.as_ref().is_some_and(|c| c.has_steering()) {
            return Ok(false);
        }
        let prefix_messages = model_context
            .get(..prefix)
            .ok_or_else(|| anyhow::anyhow!("invalid boundary runtime prefix"))?;
        let prepared = tokio::select! {biased;cancelled=context.cancelled()=>return Err(cancelled.into()),result=tokio::time::timeout(Duration::from_secs(30),journal.prepare_boundary(&commit,model_context,prefix))=>result.map_err(|_|anyhow::anyhow!("boundary preparation timed out"))??};
        let projected = prepared.projection().map(<[Message]>::to_vec);
        let compacted = commit
            .entries
            .iter()
            .any(|p| matches!(p.entry, BoundaryEntry::Compaction { .. }));
        anyhow::ensure!(
            !commit.changes_projection() || projected.is_some(),
            "projected boundary has no admitted model view"
        );
        let mut compaction_counts = None;
        let checkpoint = prepared
            .session_projection()
            .and_then(|p| p.source.summary_checkpoint.clone());

        if let Some(view) = &projected {
            anyhow::ensure!(
                view.get(..prefix) == Some(prefix_messages),
                "boundary projection changed runtime prefix"
            );
            let mut check = request.clone();
            check.messages = view.clone();
            let spec = self.provider.model_spec(&check.model)?;
            check.messages = crate::tokens::model_messages(&check.messages, &spec).into_owned();
            self.provider.validate_request(&check)?;
            let count = self.counting.request(&check, &spec);
            if compacted {
                let mut before = request.clone();
                before.messages = model_context.clone();
                let before = self
                    .counting
                    .request(&before, &spec)
                    .tokens
                    .min(usize::MAX as u64) as usize;
                compaction_counts = Some((
                    before,
                    count.tokens.min(usize::MAX as u64) as usize,
                    model_context.len().saturating_sub(view.len()),
                ));
            }
            match self.decide_context(&check, &spec, &count)? {
                crate::context_policy::ContextDecision::Reject { code } => {
                    anyhow::bail!("boundary context rejected: {code}")
                }
                crate::context_policy::ContextDecision::Compact { .. } => anyhow::ensure!(
                    self.context_compactor.is_some(),
                    "boundary context exceeds model window"
                ),
                crate::context_policy::ContextDecision::Keep => {}
            }
        }
        limits.check()?;
        if context.control.as_ref().is_some_and(|c| c.has_steering()) {
            return Ok(false);
        }
        if let Some((before, after, _)) = compaction_counts {
            events.emit(AgentRawEvent::ContextCompactionStarted {
                strategy: "native_boundary_batch".into(),
                estimated_tokens: before,
                target_tokens: after,
            });
        }
        let acknowledged=async {
            limits.check()?;
            tokio::select! {biased;cancelled=context.cancelled()=>Err(anyhow::Error::from(cancelled)),result=tokio::time::timeout(Duration::from_secs(30),journal.commit_prepared_boundary(prepared))=>result.map_err(|_|anyhow::anyhow!("lifecycle commit timed out")).and_then(|v|v).map_err(|e|anyhow::Error::from(RunFailure::from_error(RunStage::Persistence,&e)))}
        }.await;
        if let Err(error) = acknowledged {
            if compacted {
                events.emit(AgentRawEvent::ContextCompactionFailed {
                    strategy: "native_boundary_batch".into(),
                    message: "boundary append was not acknowledged".into(),
                });
            }
            return Err(error);
        }
        let message_count = messages.len();
        progress.messages.extend(messages.iter().cloned());
        if let Some(view) = projected {
            *model_context = view;
        } else {
            model_context.extend(messages);
        }
        if compacted && let Some(info) = &mut progress.model_context {
            info.compactions += 1;
            info.last_summary_checkpoint = checkpoint;
        }
        if let Some((before, after, removed)) = compaction_counts {
            events.emit(AgentRawEvent::ContextCompactionCompleted {
                strategy: "native_boundary_batch".into(),
                estimated_tokens_before: before,
                estimated_tokens_after: after,
                removed_messages: removed,
            });
        }
        Self::context_finalization_events(&commit, events);
        events.emit(AgentRawEvent::LifecycleBoundaryCommitted {
            boundary: kind,
            entries: commit.entries.len(),
            messages: message_count,
            continue_run: again,
        });
        limits.check()?;
        Ok(again)
    }
}
