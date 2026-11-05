use super::*;
use crate::extensions::*;
use std::time::Duration;
pub(super) struct BoundaryScope<'a> {
    pub kind: BoundaryKind,
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
            events,
            request,
            context,
            limits,
            journal,
        } = scope;
        limits.check()?;
        if context.control.as_ref().is_some_and(|c| c.has_steering()) {
            return Ok(false);
        }
        let mut measured = RunProgress::new(Vec::new());
        measured.usage = progress.usage.clone();
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
        let can_continue =
            progress.rounds < self.config.max_rounds && limits.remaining_tokens() != Some(0);
        let mut entries = Vec::<BoundaryProposal>::new();
        let mut again = false;
        let mut changes = Vec::new();
        let mut bytes = 0usize;
        let work = async {
            for hook in &self.boundary_hooks {
                let action =
                    crate::extensions::invoke(&hook.owner, &hook.spec, context.ui(), |cancel| {
                        hook.callback.boundary(
                            BoundaryInput {
                                workspace: self.workspace_root(),
                                kind,
                                round: progress.rounds,
                                history: &progress.messages,
                                statistics: &statistics,
                                context: model_context,
                                entries: &entries,
                                continue_requested: again,
                                can_continue,
                            },
                            cancel,
                        )
                    })
                    .await?;
                if let Some(action) = action {
                    let replaced = action.replace.is_some();
                    let appended = action.append.len();
                    let continuation = action.continue_run;
                    if let Some(replacement) = action.replace {
                        entries.clear();
                        for entry in replacement {
                            entry.validate()?;
                            entries.push(BoundaryProposal {
                                extension: hook.owner.clone(),
                                hook: hook.spec.id.clone(),
                                entry,
                            });
                        }
                    }
                    for entry in action.append {
                        entry.validate()?;
                        entries.push(BoundaryProposal {
                            extension: hook.owner.clone(),
                            hook: hook.spec.id.clone(),
                            entry,
                        });
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
                    statistics.tokens.used
                },
                requested: Some(1),
            }
            .into());
        }
        let commit = BoundaryCommit {
            startup: None,
            kind,
            round: progress.rounds,
            base_index: progress.messages.len(),
            entries,
            continue_run: again,
            changes,
        };
        crate::extensions::size(&commit, 128 * 1024)?;
        let messages = commit.messages();
        if !messages.is_empty() {
            let mut check = ModelRequest::new(&request.model, messages.clone());
            check.options = request.options.clone();
            check.tools = request.tools.clone();
            check.tool_choice = request.tool_choice.clone();
            check.stream = request.stream;
            self.provider.validate_request(&check)?;
        }
        let encoded = serde_json::to_string(&(&commit, &messages))?;
        let _ = limits.retain_text(&encoded);
        limits.check()?;
        tokio::select! {biased;cancelled=context.cancelled()=>return Err(cancelled.into()),result=tokio::time::timeout(Duration::from_secs(30),journal.commit_boundary(&commit))=>result.map_err(|_|anyhow::anyhow!("lifecycle commit timed out")).and_then(|v|v).map_err(|e|RunFailure::from_error(RunStage::Persistence,&e))?,}
        // Once committed, apply the exact canonical messages even if cancellation raced the ack.
        let message_count = messages.len();
        progress.messages.extend(messages.iter().cloned());
        model_context.extend(messages);
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
