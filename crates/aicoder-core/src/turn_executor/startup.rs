use super::*;
use crate::extensions::*;
use crate::model::ToolDefinition;
use std::time::Duration;

pub(super) struct StartupScope<'a> {
    pub trigger: Option<crate::RunTrigger>,
    pub request: &'a mut ModelRequest,
    pub messages: &'a mut Vec<Message>,
    pub prefix: &'a mut usize,
    pub progress: &'a mut RunProgress,
    pub context: &'a TurnExecutionContext,
    pub limits: &'a Arc<RunLimiter>,
    pub journal: &'a dyn crate::RunJournal,
    pub events: &'a AgentEventEmitter,
}
impl TurnExecutor {
    pub(crate) fn has_startup_hooks(&self) -> bool {
        !self.startup_hooks.is_empty()
    }
    pub(crate) async fn prepare_startup(
        &self,
        input: &mut crate::ExecutionInput,
        context: &TurnExecutionContext,
    ) -> Result<()> {
        let available = input.request.tools.clone();
        let mut entries = Vec::new();
        let mut changes = Vec::new();
        let mut boundary_changes = Vec::new();
        let mut aggregate = 0usize;
        let mut prefix_edited = false;
        let work = async {
            for hook in &self.startup_hooks {
                let action = invoke(&hook.owner, &hook.spec, context.ui(), |cancellation| {
                    hook.callback.before_start(
                        BeforeStartInput {
                            trigger: input.trigger.as_ref(),
                            workspace: self.workspace_root(),
                            request: &input.request,
                            history: &input.history,
                            system_prefix_len: input.system_prefix_len,
                            entries: &entries,
                            available_tools: &available,
                        },
                        cancellation,
                    )
                })
                .await?;
                if let Some(action) = action {
                    let mut change = StartupChange {
                        extension: hook.owner.clone(),
                        hook: hook.spec.id.clone(),
                        prefix_changed: false,
                        tools_changed: false,
                    };
                    if let Some(prefix) = action.system_prefix {
                        crate::input_context::validate_prefix(&prefix)?;
                        aggregate = aggregate.saturating_add(size(&prefix, 64 * 1024)?);
                        change.prefix_changed = prefix != input.history[..input.system_prefix_len];
                        prefix_edited = true;
                        input
                            .history
                            .splice(..input.system_prefix_len, prefix.iter().cloned());
                        input
                            .request
                            .messages
                            .splice(..input.system_prefix_len, prefix.iter().cloned());
                        input.system_prefix_len = prefix.len();
                    }
                    if let Some(names) = action.tools {
                        anyhow::ensure!(
                            names.len() <= available.len(),
                            "startup tool selection exceeds its admitted catalog"
                        );
                        aggregate = aggregate.saturating_add(size(&names, 64 * 1024)?);
                        let mut tools = Vec::new();
                        for name in names {
                            anyhow::ensure!(
                                !tools.iter().any(|t: &ToolDefinition| t.name == name),
                                "duplicate startup tool selection"
                            );
                            tools.push(available.iter().find(|t| t.name == name).ok_or_else(||anyhow::anyhow!("startup selected a tool outside the admitted root catalog"))?.clone());
                        }
                        change.tools_changed = tools != input.request.tools;
                        input.request.tools = tools;
                    }
                    let appended = action.append.len();
                    anyhow::ensure!(
                        appended <= 32usize.saturating_sub(entries.len()),
                        "startup entry count exceeds 32"
                    );
                    for entry in action.append {
                        entry.validate()?;
                        let proposal = BoundaryProposal {
                            extension: hook.owner.clone(),
                            hook: hook.spec.id.clone(),
                            entry,
                        };
                        if let BoundaryEntry::Context { kind, text } = &proposal.entry {
                            let message = crate::session::NativeContextSource {
                                extension: proposal.extension.clone(),
                                hook: proposal.hook.clone(),
                                kind: kind.clone(),
                            }
                            .message(text);
                            input.history.push(message.clone());
                            input.request.messages.push(message);
                        }
                        entries.push(proposal);
                    }
                    anyhow::ensure!(entries.len() <= 32, "startup entry count exceeds 32");
                    aggregate = aggregate.saturating_add(size(&entries, MAX_BOUNDARY_BYTES)?);
                    anyhow::ensure!(
                        aggregate <= 256 * 1024,
                        "startup chain exceeds aggregate data budget"
                    );
                    boundary_changes.push(BoundaryChange {
                        extension: hook.owner.clone(),
                        hook: hook.spec.id.clone(),
                        replaced: false,
                        appended,
                        continue_run: None,
                    });
                    changes.push(change);
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::select! { biased;
            cancelled=context.cancelled()=>return Err(cancelled.into()),
            result=tokio::time::timeout(Duration::from_secs(30),work)=>result.map_err(|_|anyhow::anyhow!("startup hook phase timed out"))??,
        }
        let count = entries
            .iter()
            .filter(|p| matches!(p.entry, BoundaryEntry::Context { .. }))
            .count();
        let mut commit = BoundaryCommit {
            message_finalizations: Vec::new(),
            startup: Some(StartupMetadata {
                system_prefix_messages: input.system_prefix_len,
                tools: input.request.tools.iter().map(|t| t.name.clone()).collect(),
                changes,
            }),
            kind: BoundaryKind::BeforeStart,
            round: 0,
            base_index: input.history.len() - count,
            entries,
            continue_run: false,
            changes: boundary_changes,
        };
        self.finalize_context_commit(&mut commit, context).await?;
        if !commit.message_finalizations.is_empty() {
            let messages = commit.messages();
            let n = messages.len();
            input
                .history
                .splice(input.history.len() - n.., messages.iter().cloned());
            input
                .request
                .messages
                .splice(input.request.messages.len() - n.., messages);
        }
        size(&commit, 128 * 1024)?;
        let prefix = prefix_edited.then(|| &input.request.messages[..input.system_prefix_len]);
        size(&(&commit, commit.messages(), prefix), 256 * 1024)?;
        let encoded = serde_json::to_string(&(&commit, commit.messages(), prefix))?;
        if let Some(limit) = self.config.budget.max_output_bytes {
            anyhow::ensure!(
                encoded.len() <= limit,
                BudgetExceeded {
                    resource: BudgetResource::OutputBytes,
                    limit: limit as u64,
                    used: 0,
                    requested: Some(encoded.len() as u64)
                }
            );
        }
        input.prepared_start = Some(PreparedStartup {
            owner: self.request_owner,
            commit,
            encoded,
            committed: false,
        });
        input.prepared_request = None;
        Ok(())
    }
    pub(super) async fn apply_startup(
        &self,
        (prepared, prepared_view): (Option<PreparedStartup>, &mut Option<PreparedRequestView>),
        scope: StartupScope<'_>,
    ) -> Result<()> {
        let StartupScope {
            trigger,
            request,
            messages,
            prefix,
            progress,
            context,
            limits,
            journal,
            events,
        } = scope;
        if self.startup_hooks.is_empty() && prepared.is_none() {
            return Ok(());
        }
        let (prepared, view) = if let Some(prepared) = prepared {
            anyhow::ensure!(
                prepared.owner == self.request_owner,
                "startup snapshot belongs to another executor"
            );
            (prepared, None)
        } else {
            let mut input = crate::ExecutionInput::from_request(request.clone());
            input.request.messages = messages.clone();
            input.history = progress.messages.clone();
            input.system_prefix_len = *prefix;
            input.trigger = trigger;
            self.prepare_startup(&mut input, context).await?;
            let prepared = input.prepared_start.take().expect("prepared startup");
            (prepared, Some(input))
        };
        let _ = limits.retain_text(&prepared.encoded);
        limits.check()?;
        if !prepared.committed {
            tokio::select! { biased;
                cancelled=context.cancelled()=>return Err(cancelled.into()),
                result=tokio::time::timeout(Duration::from_secs(30),journal.commit_boundary(&prepared.commit))=>result.map_err(|_|anyhow::anyhow!("startup commit timed out")).and_then(|v|v).map_err(|e|RunFailure::from_error(RunStage::Persistence,&e))?,
            }
        }
        if let Some(input) = view {
            *prefix = input.system_prefix_len;
            *messages = input.request.messages;
            request.tools = input.request.tools;
            progress.messages = input.history;
            progress.input_messages = prepared.commit.base_index;
            *prepared_view = None;
        }
        Self::context_finalization_events(&prepared.commit, events);
        events.emit(AgentRawEvent::LifecycleBoundaryCommitted {
            boundary: BoundaryKind::BeforeStart,
            entries: prepared.commit.entries.len(),
            messages: prepared.commit.messages().len(),
            continue_run: false,
        });
        limits.check()?;
        Ok(())
    }
}
