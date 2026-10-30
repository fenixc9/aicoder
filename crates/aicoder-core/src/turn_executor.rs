//! Executes one agent turn through its model, tool, and completion-verification rounds.

use std::{path::PathBuf, sync::Arc};

#[cfg(test)]
use crate::model::{ModelOptions, ModelResponse, ModelUsage};
use anyhow::{Context, Result};

use crate::{
    cancellation::{TurnCancelled, TurnExecutionContext},
    completion::{
        AcceptAllCompletionVerifier, CompletionContext, CompletionVerdict, CompletionVerifier,
    },
    context::{ContextCompaction, ContextCompactionInput, ContextCompactor},
    events::{AgentEventEmitter, AgentEventHandler, AgentRawEvent, AgentStage, RoundOutcome},
    model::{Message, ModelProvider, ModelRequest, ModelToolChoice, StopReason},
    run::{
        BudgetExceeded, BudgetResource, InterruptedTurn, ModelStopped, RunBudget, RunFailure,
        RunLimiter, RunOutcome, RunProgress, RunStage, RunStop, TurnExecutionResult,
    },
    state::{AgentRunState, AgentRunStateMachine},
    tools::{ApprovalHandler, DenyAllApproval, DispatcherConfig, ToolDispatcher, ToolRegistry},
};

#[derive(Debug, Clone)]
pub struct TurnExecutionConfig {
    pub max_rounds: usize,
    pub stream: bool,
    pub budget: RunBudget,
    pub events: crate::events::EventDeliveryConfig,
}

impl Default for TurnExecutionConfig {
    fn default() -> Self {
        Self {
            max_rounds: 8,
            stream: true,
            budget: RunBudget::default(),
            events: crate::events::EventDeliveryConfig::default(),
        }
    }
}

pub(crate) struct RunScope {
    pub(crate) events: AgentEventEmitter,
    state: AgentRunStateMachine,
}
pub struct TurnExecutor {
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) dispatcher: Arc<ToolDispatcher>,
    completion_verifier: Arc<dyn CompletionVerifier>,
    pub(crate) context_compactor: Option<Arc<dyn ContextCompactor>>,
    pub(crate) input_context: Option<Arc<dyn crate::InputContextProvider>>,
    pub(crate) config: TurnExecutionConfig,
    pub(crate) counting: crate::tokens::TokenCounting,
    pub(crate) context_policy: Arc<dyn crate::context_policy::ContextPolicy>,
}

impl TurnExecutor {
    pub fn builder<P>(provider: P) -> TurnExecutorBuilder
    where
        P: ModelProvider + 'static,
    {
        TurnExecutorBuilder::new(provider)
    }

    pub fn builder_from_shared(provider: Arc<dyn ModelProvider>) -> TurnExecutorBuilder {
        TurnExecutorBuilder::from_shared(provider)
    }

    pub fn new(
        provider: Arc<dyn ModelProvider>,
        dispatcher: Arc<ToolDispatcher>,
        config: TurnExecutionConfig,
    ) -> Self {
        Self::new_with_verifier(
            provider,
            dispatcher,
            Arc::new(AcceptAllCompletionVerifier),
            None,
            None,
            config,
        )
    }

    fn new_with_verifier(
        provider: Arc<dyn ModelProvider>,
        dispatcher: Arc<ToolDispatcher>,
        completion_verifier: Arc<dyn CompletionVerifier>,
        context_compactor: Option<Arc<dyn ContextCompactor>>,
        input_context: Option<Arc<dyn crate::InputContextProvider>>,
        config: TurnExecutionConfig,
    ) -> Self {
        Self {
            provider,
            dispatcher,
            completion_verifier,
            context_compactor,
            input_context,
            config,
            counting: crate::tokens::TokenCounting::default(),
            context_policy: Arc::new(crate::context_policy::WindowContextPolicy),
        }
    }

    pub fn workspace_root(&self) -> &std::path::Path {
        self.dispatcher.workspace_root()
    }

    pub async fn run(&self, request: ModelRequest) -> Result<TurnExecutionResult> {
        self.run_with_context(request, Arc::new(()), TurnExecutionContext::new())
            .await
    }

    pub(crate) async fn compact_saved_session(
        &self,
        repository: &dyn crate::session::SessionRepository,
        id: &str,
        mut template: ModelRequest,
        context: TurnExecutionContext,
    ) -> crate::summary::ManualCompactionReport {
        template.tools = self.dispatcher.registry().definitions();
        let spec = self
            .provider
            .model_spec(&template.model)
            .unwrap_or_else(|_| crate::model::ModelSpec::new("unavailable", &template.model));
        let compaction_input = ContextCompactionInput {
            model: &template.model,
            messages: &template.messages,
            tools: Some(&template.tools),
            max_output_tokens: template.options.max_output_tokens,
        };
        let target_tokens = self
            .context_compactor
            .as_ref()
            .map_or(usize::MAX, |compactor| {
                compactor.target_tokens(compaction_input)
            });
        let measure = crate::context::ContextMeasure {
            counting: self.counting.clone(),
            spec,
            response_format: template.options.response_format.clone(),
            tool_choice: template.tool_choice.clone(),
            target_tokens,
        };
        crate::summary::manual::compact(
            crate::summary::manual::ManualHost {
                compactor: self.context_compactor.clone(),
                input_context: self.input_context.clone(),
                workspace: self.workspace_root(),
                budget: self.config.budget.clone(),
                measure,
                policy: self.context_policy.clone(),
            },
            repository,
            id,
            template,
            context,
        )
        .await
    }
    pub async fn run_input(&self, input: crate::ExecutionInput) -> Result<TurnExecutionResult> {
        self.run_input_outcome(input, Arc::new(()), TurnExecutionContext::new())
            .await
            .into_result()
    }
    pub async fn run_input_outcome(
        &self,
        input: crate::ExecutionInput,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
    ) -> RunOutcome {
        self.run_input_with_journal(input, handler, context, &())
            .await
    }
    pub async fn run_input_controlled(
        &self,
        input: crate::ExecutionInput,
        handler: Arc<dyn AgentEventHandler>,
        control: crate::RunControl,
    ) -> RunOutcome {
        self.run_input_outcome(input, handler, control.context.clone())
            .await
    }
    pub async fn run_input_with_journal(
        &self,
        input: crate::ExecutionInput,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
        journal: &dyn crate::RunJournal,
    ) -> RunOutcome {
        let scope = self.start_scope(&input.request.model, handler, &context);
        self.run_scoped_input(input, context, journal, scope).await
    }
    pub async fn run_with_handler(
        &self,
        request: ModelRequest,
        handler: Arc<dyn AgentEventHandler>,
    ) -> Result<TurnExecutionResult> {
        self.run_with_context(request, handler, TurnExecutionContext::new())
            .await
    }

    pub async fn run_with_context(
        &self,
        request: ModelRequest,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
    ) -> Result<TurnExecutionResult> {
        self.run_outcome(request, handler, context)
            .await
            .into_result()
    }

    pub async fn run_outcome(
        &self,
        request: ModelRequest,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
    ) -> RunOutcome {
        self.run_with_journal(request, handler, context, &()).await
    }
    /// Consume the unique execution side while the cloneable handle remains with the host.
    pub async fn run_controlled(
        &self,
        request: ModelRequest,
        handler: Arc<dyn AgentEventHandler>,
        control: crate::RunControl,
    ) -> RunOutcome {
        self.run_with_journal(request, handler, control.context.clone(), &())
            .await
    }
    pub async fn run_controlled_with_journal(
        &self,
        request: ModelRequest,
        handler: Arc<dyn AgentEventHandler>,
        control: crate::RunControl,
        journal: &dyn crate::RunJournal,
    ) -> RunOutcome {
        self.run_with_journal(request, handler, control.context.clone(), journal)
            .await
    }
    fn count_partial(
        &self,
        spec: &crate::model::ModelSpec,
        partial: &crate::run::PartialModelResponse,
    ) -> u64 {
        let mut count = self
            .counting
            .text(spec, &partial.text)
            .tokens
            .saturating_add(self.counting.text(spec, &partial.reasoning).tokens);
        for call in &partial.tool_calls {
            count = count
                .saturating_add(self.counting.text(spec, &call.arguments).tokens)
                .saturating_add(self.counting.text(spec, &call.name).tokens);
        }
        count
    }
    pub fn model_spec(
        &self,
        model: &str,
    ) -> std::result::Result<crate::model::ModelSpec, crate::model::ProviderError> {
        self.provider.model_spec(model)
    }
    pub async fn run_with_journal(
        &self,
        request: ModelRequest,
        handler: Arc<dyn AgentEventHandler>,
        context: TurnExecutionContext,
        journal: &dyn crate::RunJournal,
    ) -> RunOutcome {
        let scope = self.start_scope(&request.model, handler, &context);
        self.run_scoped(request, context, journal, scope).await
    }
    pub(crate) fn start_scope(
        &self,
        model: &str,
        handler: Arc<dyn AgentEventHandler>,
        context: &TurnExecutionContext,
    ) -> RunScope {
        if let Some(control) = &context.control {
            control.start();
        }
        let source = context.control.as_ref().map_or_else(
            || crate::events::EventSource::new(crate::RunId::new()),
            |control| control.event_source(),
        );
        let events = AgentEventEmitter::with_source(handler, source, self.config.events.clone());
        events.emit(AgentRawEvent::AgentStarted {
            model: model.into(),
        });
        let mut state = AgentRunStateMachine::new();
        transition_state(&mut state, &events, AgentRunState::Preparing)
            .expect("new run can prepare");
        RunScope { events, state }
    }
    pub(crate) async fn run_scoped(
        &self,
        request: ModelRequest,
        context: TurnExecutionContext,
        journal: &dyn crate::RunJournal,
        scope: RunScope,
    ) -> RunOutcome {
        self.run_scoped_input(
            crate::ExecutionInput::from_request(request),
            context,
            journal,
            scope,
        )
        .await
    }
    pub(crate) async fn run_scoped_input(
        &self,
        input: crate::ExecutionInput,
        context: TurnExecutionContext,
        journal: &dyn crate::RunJournal,
        scope: RunScope,
    ) -> RunOutcome {
        let crate::ExecutionInput {
            request,
            history,
            system_prefix_len,
            source,
            prepared_context,
            started,
        } = input;
        let context = context.child();
        let mut limiter = RunLimiter::new(self.config.budget.clone(), context.clone());
        if let Some(started) = started {
            limiter.started = started;
        }
        let limits = Arc::new(limiter);
        let mut progress = RunProgress::new(history);
        progress.input_context = prepared_context;
        progress.model_context = Some(crate::ModelContextInfo {
            source,
            initial_history_messages: progress.messages.len(),
            initial_context_messages: request.messages.len(),
            requests: 0,
            last_request_messages: None,
            last_input_token_estimate: None,
            compactions: 0,
            summary_attempts: 0,
            summary_fallbacks: 0,
            last_summary_checkpoint: None,
            last_input_count: None,
            context_count: None,
            context_policy: None,
            context_decision: None,
        });
        let RunScope { events, mut state } = scope;
        let mut execution = match self.config.events.validate() {
            Err(error) => Err(error),
            Ok(())
                if limits
                    .budget
                    .max_duration
                    .is_some_and(|duration| limits.started.checked_add(duration).is_none()) =>
            {
                Err(anyhow::anyhow!("run duration is too large"))
            }
            Ok(()) => {
                let inner = self.run_inner(
                    (request, system_prefix_len),
                    &events,
                    &mut state,
                    &context,
                    &mut progress,
                    (&limits, journal),
                );
                tokio::pin!(inner);
                match limits.budget.max_duration {
                    Some(duration) => tokio::select! {
                        biased;
                        _=tokio::time::sleep_until(limits.started+duration)=>{
                            limits.stop(BudgetExceeded {resource:BudgetResource::Duration,limit:crate::run::millis(duration),used:crate::run::millis(limits.started.elapsed()),requested:None});
                            inner.await
                        },
                        result=&mut inner=>result,
                    },
                    None => inner.await,
                }
            }
        };
        let preserve_failure = limits.violation().is_none()
            && execution.as_ref().is_err_and(|error| {
                error.downcast_ref::<TurnCancelled>().is_none()
                    && error.downcast_ref::<BudgetExceeded>().is_none()
                    && error.downcast_ref::<ModelStopped>().is_none()
            });
        if !preserve_failure && let Err(error) = limits.check() {
            execution = Err(error);
        }
        limits.snapshot(&mut progress);
        progress.event_delivery = events.delivery_report();
        if let Some(control) = &context.control {
            progress.pending_inputs = control.seal();
        }
        let mut outcome = match execution {
            Ok((final_message, finish_reason)) => RunOutcome::Completed(TurnExecutionResult {
                final_message,
                finish_reason,
                progress,
            }),
            Err(error) => {
                let stop = classify_stop(&error, &limits, progress.stage);
                RunOutcome::Interrupted(InterruptedTurn {
                    stop,
                    progress,
                    persistence_error: None,
                })
            }
        };
        if let Err(error) = journal.finish(&outcome).await {
            let failure = error
                .downcast_ref::<RunFailure>()
                .cloned()
                .unwrap_or_else(|| RunFailure::from_error(RunStage::Persistence, &error));
            outcome = match outcome {
                RunOutcome::Completed(result) => RunOutcome::Interrupted(InterruptedTurn {
                    stop: RunStop::Failed(failure.clone()),
                    progress: result.progress,
                    persistence_error: Some(Box::new(failure)),
                }),
                RunOutcome::Interrupted(mut turn) => {
                    turn.persistence_error = Some(Box::new(failure));
                    RunOutcome::Interrupted(turn)
                }
            };
        }
        self.finish_scope(RunScope { events, state }, outcome, &context)
            .await
    }
    pub(crate) async fn finish_preparation(
        &self,
        scope: RunScope,
        mut outcome: RunOutcome,
        context: &TurnExecutionContext,
    ) -> RunOutcome {
        let progress = outcome.progress_mut();
        if let Some(control) = &context.control {
            progress.pending_inputs = control.seal();
        }
        progress.event_delivery = scope.events.delivery_report();
        self.finish_scope(scope, outcome, context).await
    }
    async fn finish_scope(
        &self,
        scope: RunScope,
        mut outcome: RunOutcome,
        context: &TurnExecutionContext,
    ) -> RunOutcome {
        let RunScope { events, mut state } = scope;
        if let Some(control) = &context.control {
            control.finish();
        }
        events.close_round(&outcome);
        let progress = outcome.progress();
        let round = (progress.rounds > 0).then_some(progress.rounds);
        let terminal = match outcome.stop() {
            None => AgentRunState::Completed {
                rounds: progress.rounds,
            },
            Some(RunStop::Cancelled { .. }) => AgentRunState::Aborted { round },
            Some(RunStop::Budget(_) | RunStop::Model(_)) => AgentRunState::Stopped { round },
            Some(RunStop::Failed(_)) => AgentRunState::Failed { round },
        };
        if let Err(error) = transition_state(&mut state, &events, terminal) {
            let progress = match outcome {
                RunOutcome::Completed(result) => result.progress,
                RunOutcome::Interrupted(turn) => turn.progress,
            };
            outcome = RunOutcome::Interrupted(InterruptedTurn {
                stop: RunStop::Failed(RunFailure::from_error(progress.stage, &error)),
                progress,
                persistence_error: None,
            });
        }
        events.set_termination(&outcome);
        match &outcome {
            RunOutcome::Completed(result) => events.emit(AgentRawEvent::AgentCompleted {
                rounds: result.rounds,
                usage: result.usage.clone().into(),
            }),
            RunOutcome::Interrupted(turn) => match &turn.stop {
                RunStop::Cancelled { reason } => events.emit(AgentRawEvent::AgentAborted {
                    reason: reason.clone().into(),
                }),
                RunStop::Budget(_) | RunStop::Model(_) => {
                    events.emit(AgentRawEvent::AgentStopped {
                        stop: Arc::new(turn.stop.clone()),
                    })
                }
                RunStop::Failed(_) => events.emit(AgentRawEvent::AgentFailed {
                    stage: AgentStage::Turn,
                    message: turn.stop.to_string().into(),
                }),
            },
        }
        context.cancel("run finished");
        let delivery = events.shutdown().await;
        match &mut outcome {
            RunOutcome::Completed(result) => result.progress.event_delivery = delivery,
            RunOutcome::Interrupted(turn) => turn.progress.event_delivery = delivery,
        }
        outcome
    }

    async fn run_inner(
        &self,
        (mut request, system_prefix_len): (ModelRequest, usize),
        events: &AgentEventEmitter,
        state: &mut AgentRunStateMachine,
        context: &TurnExecutionContext,
        progress: &mut RunProgress,
        control: (&Arc<RunLimiter>, &dyn crate::RunJournal),
    ) -> Result<(Message, Option<StopReason>)> {
        let (limits, journal) = control;
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        limits.check()?;

        let mut context_messages = std::mem::take(&mut request.messages);
        if progress.input_context.is_none()
            && let Some(provider) = &self.input_context
        {
            let prepared = tokio::select! {
                biased;
                cancelled=context.cancelled()=>return Err(cancelled.into()),
                prepared=provider.prepare(self.workspace_root(),&progress.messages)=>prepared?,
            };
            crate::input_context::validate_prefix(&prepared.system_prefix)?;
            let history = progress.messages.split_off(system_prefix_len);
            let projected_history = context_messages.split_off(system_prefix_len);
            context_messages = prepared.system_prefix.clone();
            context_messages.extend(projected_history);
            progress.messages = prepared.system_prefix;
            progress.messages.extend(history);
            progress.input_messages = progress.messages.len();
            progress.input_context = Some(prepared.provenance);
            if let Some(info) = &mut progress.model_context {
                info.initial_history_messages = progress.messages.len();
                info.initial_context_messages = context_messages.len();
            }
        }
        if progress.input_context.is_some() {
            tokio::select! {
                biased;
                cancelled=context.cancelled()=>return Err(cancelled.into()),
                result=journal.record_input_context(progress.input_context.as_ref().expect("prepared context"))=>{
                    result.map_err(|error|anyhow::Error::from(RunFailure::from_error(RunStage::Persistence,&error)))?;
                }
            }
        }

        request.stream = self.config.stream;
        let definitions = self.dispatcher.registry().definitions();
        if request.tools.is_empty() {
            request.tools = definitions;
        } else {
            anyhow::ensure!(
                request.tools.iter().all(|tool| definitions.contains(tool)),
                "request tools must match registered tool definitions"
            );
        }

        if let Some(info) = &mut progress.model_context {
            info.initial_history_messages = progress.messages.len();
            info.initial_context_messages = context_messages.len();
        }

        for round in 1..=self.config.max_rounds {
            limits.check()?;
            progress.rounds = round;
            let round_events = events.for_round(round);
            round_events.emit(AgentRawEvent::RoundStarted);
            transition_state(state, &round_events, AgentRunState::AwaitingModel { round })?;
            apply_inputs(
                &mut context_messages,
                progress,
                context,
                journal,
                InputBoundary::BeforeModel,
                &round_events,
            )
            .await?;
            progress.stage = RunStage::Compaction;
            let context_spec = self.provider.model_spec(&request.model)?;
            let decision_messages = crate::tokens::model_messages(&context_messages, &context_spec);
            let measured = self
                .counting
                .request_view(&request, &context_spec, &decision_messages);
            let decision = match self.decide_context_view(
                &request,
                &decision_messages,
                &context_spec,
                &measured,
            ) {
                Ok(decision) => decision,
                Err(error) => {
                    round_events.emit(AgentRawEvent::RoundCompleted {
                        outcome: RoundOutcome::Failed,
                    });
                    return Err(error);
                }
            };
            if let Some(info) = &mut progress.model_context {
                info.context_count = Some(measured.clone());
                info.context_policy = Some(self.context_policy.name().into());
                info.context_decision = Some(decision.clone());
            }
            if let crate::context_policy::ContextDecision::Reject { code } = &decision {
                round_events.emit(AgentRawEvent::RoundCompleted {
                    outcome: RoundOutcome::Failed,
                });
                anyhow::bail!("context policy rejected request: {code}");
            }
            drop(decision_messages);
            if let Some(compactor) = &self.context_compactor {
                let input = ContextCompactionInput {
                    model: &request.model,
                    messages: &context_messages,
                    tools: Some(request.tools.as_slice()),
                    max_output_tokens: request.options.max_output_tokens,
                };
                let estimated_tokens = if compactor.supports_counting() {
                    measured.tokens.min(usize::MAX as u64) as usize
                } else {
                    compactor.estimate_tokens(input)
                };
                if let crate::context_policy::ContextDecision::Compact { target_tokens } = decision
                {
                    let target_tokens = target_tokens.min(usize::MAX as u64) as usize;
                    transition_state(state, &round_events, AgentRunState::Compacting { round })?;
                    round_events.emit(AgentRawEvent::ContextCompactionStarted {
                        strategy: compactor.name().into(),
                        estimated_tokens,
                        target_tokens,
                    });
                    progress.stage = RunStage::Compaction;
                    if compactor.uses_model() && !compactor.manages_budget() {
                        ensure_token_capacity(limits, estimated_tokens as u64)?;
                    }
                    let recorded = compactor
                        .compact_counted(
                            crate::summary::CompactionRequest {
                                input,
                                history: &progress.messages,
                                system_prefix_len: progress
                                    .messages
                                    .iter()
                                    .take_while(|message| {
                                        message.role == crate::model::MessageRole::System
                                    })
                                    .count(),
                                force: false,
                                token_budget: limits.remaining_tokens(),
                                archived_messages: progress
                                    .model_context
                                    .as_ref()
                                    .and_then(|info| info.source.as_ref())
                                    .map_or(0, |source| source.archived_messages),
                            },
                            crate::context::ContextMeasure {
                                counting: self.counting.clone(),
                                spec: context_spec.clone(),
                                response_format: request.options.response_format.clone(),
                                tool_choice: request.tool_choice.clone(),
                                target_tokens,
                            },
                            context.clone(),
                        )
                        .await
                        .and_then(|recorded| {
                            let prefix = progress
                                .messages
                                .iter()
                                .take_while(|message| {
                                    message.role == crate::model::MessageRole::System
                                })
                                .count();
                            let validated =
                                ensure_valid_compaction(&recorded.compaction, target_tokens)
                                    .and_then(|()| {
                                        anyhow::ensure!(
                                            recorded.compaction.messages.get(..prefix)
                                                == input.messages.get(..prefix),
                                            "context compactor changed the protected system prefix"
                                        );
                                        Ok(())
                                    });
                            if let Err(error) = validated {
                                return Err(crate::summary::SummaryFailure {
                                    kind: crate::summary::SummaryFailureKind::InvalidResponse,
                                    message: error.to_string(),
                                    usage: recorded.compaction.usage.clone(),
                                    tokens: recorded.accounting.clone().unwrap_or_else(|| {
                                        crate::run::TokenAccounting::from_usage(
                                            &recorded.compaction.usage,
                                            (estimated_tokens as u64)
                                                .saturating_add(target_tokens as u64),
                                        )
                                    }),
                                    attempts: recorded.attempts,
                                }
                                .into());
                            }
                            Ok(recorded)
                        });
                    let recorded = match recorded {
                        Ok(compacted) => compacted,
                        Err(error) if error.downcast_ref::<TurnCancelled>().is_some() => {
                            if compactor.uses_model() {
                                progress
                                    .usage
                                    .accumulate(&crate::model::ModelUsage::default());
                                limits.account_tokens(
                                    &crate::model::ModelUsage::default(),
                                    (estimated_tokens as u64).saturating_add(target_tokens as u64),
                                );
                            }
                            round_events.emit(AgentRawEvent::ContextCompactionFailed {
                                strategy: compactor.name().into(),
                                message: context.reason(),
                            });
                            round_events.emit(AgentRawEvent::RoundCompleted {
                                outcome: RoundOutcome::Aborted,
                            });
                            return Err(error);
                        }
                        Err(error) => {
                            if let Some(failure) =
                                error.downcast_ref::<crate::summary::SummaryFailure>()
                            {
                                if let Some(info) = &mut progress.model_context {
                                    info.summary_attempts += failure.attempts;
                                }
                                progress.usage.accumulate(&failure.usage);
                                progress.reported_usage |=
                                    crate::run::usage_reported(&failure.usage);
                                limits.account_compaction(&failure.tokens);
                                round_events.emit(AgentRawEvent::ContextCompactionFailed {
                                    strategy: compactor.name().into(),
                                    message: failure.message.clone().into(),
                                });
                                if failure.kind == crate::summary::SummaryFailureKind::Budget {
                                    limits.snapshot(progress);
                                    limits.stop(BudgetExceeded {
                                        resource: BudgetResource::Tokens,
                                        limit: limits.budget.max_tokens.unwrap_or(0),
                                        used: progress.tokens.used,
                                        requested: None,
                                    });
                                }
                                let cancelled = context.is_cancelled()
                                    || failure.kind
                                        == crate::summary::SummaryFailureKind::Cancelled;
                                round_events.emit(AgentRawEvent::RoundCompleted {
                                    outcome: if cancelled {
                                        RoundOutcome::Aborted
                                    } else {
                                        RoundOutcome::Failed
                                    },
                                });
                                return Err(if cancelled {
                                    context.error().into()
                                } else {
                                    error
                                });
                            }
                            if compactor.uses_model() {
                                progress
                                    .usage
                                    .accumulate(&crate::model::ModelUsage::default());
                                limits.account_tokens(
                                    &crate::model::ModelUsage::default(),
                                    (estimated_tokens as u64).saturating_add(target_tokens as u64),
                                );
                            }
                            round_events.emit(AgentRawEvent::ContextCompactionFailed {
                                strategy: compactor.name().into(),
                                message: format!("{error:#}").into(),
                            });
                            round_events.emit(AgentRawEvent::RoundCompleted {
                                outcome: RoundOutcome::Failed,
                            });
                            return Err(error).with_context(|| {
                                format!("Context compaction failed in agent round {round}")
                            });
                        }
                    };
                    if let Some(info) = &mut progress.model_context {
                        info.summary_attempts += recorded.attempts;
                        info.summary_fallbacks += usize::from(!recorded.diagnostics.is_empty());
                    }
                    for message in &recorded.diagnostics {
                        round_events.emit(AgentRawEvent::ContextCompactionFailed {
                            strategy: compactor.name().into(),
                            message: message.clone().into(),
                        });
                    }
                    let compacted = recorded.compaction;
                    if compacted.usage.total_tokens != Some(0) {
                        round_events.emit(AgentRawEvent::UsageUpdated {
                            usage: compacted.usage.clone().into(),
                        });
                    }
                    progress.reported_usage |= compacted.usage != crate::model::ModelUsage::zero()
                        && crate::run::usage_reported(&compacted.usage);
                    progress.usage.accumulate(&compacted.usage);
                    if let Some(accounting) = &recorded.accounting {
                        limits.account_compaction(accounting);
                    } else {
                        limits.account_tokens(
                            &compacted.usage,
                            (compacted.estimated_tokens_before as u64)
                                .saturating_add(compacted.estimated_tokens_after as u64),
                        );
                    }
                    if let Some(draft) = &recorded.checkpoint {
                        progress.stage = RunStage::Persistence;
                        let checkpoint = tokio::select! {biased;cancelled=context.cancelled()=>Err(anyhow::Error::from(cancelled)),result=journal.record_summary(draft,progress.input_messages,progress.messages.len())=>result};
                        match checkpoint {
                            Ok(id) => {
                                if let Some(info) = &mut progress.model_context {
                                    info.last_summary_checkpoint = id;
                                }
                            }
                            Err(error) => {
                                round_events.emit(AgentRawEvent::ContextCompactionFailed {
                                    strategy: compactor.name().into(),
                                    message: "summary checkpoint could not be committed".into(),
                                });
                                let cancelled = context.is_cancelled();
                                round_events.emit(AgentRawEvent::RoundCompleted {
                                    outcome: if cancelled {
                                        RoundOutcome::Aborted
                                    } else {
                                        RoundOutcome::Failed
                                    },
                                });
                                return Err(if cancelled {
                                    context.error().into()
                                } else {
                                    anyhow::Error::from(RunFailure::from_error(
                                        RunStage::Persistence,
                                        &error,
                                    ))
                                });
                            }
                        }
                    }
                    context_messages = compacted.messages;
                    if let Some(info) = &mut progress.model_context {
                        info.compactions += 1;
                    }
                    round_events.emit(AgentRawEvent::ContextCompactionCompleted {
                        strategy: compactor.name().into(),
                        estimated_tokens_before: compacted.estimated_tokens_before,
                        estimated_tokens_after: compacted.estimated_tokens_after,
                        removed_messages: compacted.removed_messages,
                    });
                    transition_state(state, &round_events, AgentRunState::AwaitingModel { round })?;
                }
            }
            progress.stage = RunStage::Model;
            let model_spec = self
                .provider
                .model_spec(&request.model)
                .map_err(anyhow::Error::from)?;
            request.messages = context_messages
                .iter()
                .filter_map(|message| {
                    let mut message = message.clone();
                    if message.role == crate::model::MessageRole::Assistant
                        && !model_spec.capabilities.reasoning_history
                    {
                        message.content.retain(|block| {
                            !matches!(block, crate::model::ContentBlock::Reasoning { .. })
                        });
                        if message.content.is_empty() {
                            return None;
                        }
                    }
                    Some(message)
                })
                .collect();
            self.provider
                .validate_request(&request)
                .map_err(anyhow::Error::from)?;
            progress.stage = RunStage::Model;
            let input_count = self.counting.request(&request, &model_spec);
            let input_estimate = input_count.tokens;
            ensure_token_capacity(limits, input_estimate)?;
            let original_output_limit = request.options.max_output_tokens;
            let mut budget_clamped = false;
            if let Some(remaining) = limits.remaining_tokens() {
                let model_limit = original_output_limit
                    .or(model_spec.max_output_tokens)
                    .unwrap_or(u32::MAX);
                let output = (remaining - input_estimate).min(u64::from(model_limit)) as u32;
                if request
                    .options
                    .max_output_tokens
                    .is_none_or(|original| output < original)
                {
                    request.options.max_output_tokens = Some(output);
                    budget_clamped = output < model_limit;
                }
            }
            let sink = Arc::new(crate::events::RunModelEventSink::with_limits(
                round_events.clone(),
                limits.clone(),
                input_estimate
                    .saturating_add(u64::from(request.options.max_output_tokens.unwrap_or(0))),
            ));
            round_events.emit(AgentRawEvent::ModelRequestStarted);
            if let Some(info) = &mut progress.model_context {
                info.requests += 1;
                info.last_request_messages = Some(request.messages.len());
                info.last_input_token_estimate = Some(input_estimate);
                info.last_input_count = Some(input_count);
            }
            let messages = std::mem::take(&mut request.messages);
            let mut model_request = request.clone();
            model_request.messages = messages;
            let response = match tokio::select! {
                biased;
                cancelled = context.cancelled() => Err(anyhow::Error::from(cancelled)),
                result = self.provider.generate(model_request, sink.clone(), context.clone()) =>
                    result.map_err(|error| if error.kind == crate::model::ProviderErrorKind::Cancelled {
                        context.cancel(error.message);
                        anyhow::Error::from(context.error())
                    } else { anyhow::Error::from(error) }),
            }.with_context(|| format!("Model request failed in agent round {round}"))
            {
                Ok(response) => response,
                Err(error) if error.downcast_ref::<TurnCancelled>().is_some() => {
                    let (usage,partial)=sink.abort(&context.reason());
                    progress.reported_usage|=crate::run::usage_reported(&usage);progress.usage.accumulate(&usage);limits.account_tokens(&usage,input_estimate.saturating_add(self.count_partial(&model_spec,&partial)));
                    progress.partial_response=(!partial.is_empty()).then_some(partial);
                    round_events.emit(AgentRawEvent::ModelResponseFailed {
                        message: context.reason(),
                    });
                    round_events.emit(AgentRawEvent::RoundCompleted {
                        outcome: RoundOutcome::Aborted,
                    });
                    return Err(error);
                }
                Err(error) => {
                    let (usage,partial)=sink.abort(&error.to_string());
                    progress.reported_usage|=crate::run::usage_reported(&usage);progress.usage.accumulate(&usage);limits.account_tokens(&usage,input_estimate.saturating_add(self.count_partial(&model_spec,&partial)));
                    progress.partial_response=(!partial.is_empty()).then_some(partial);
                    round_events.emit(AgentRawEvent::ModelResponseFailed {
                        message: format!("{error:#}").into(),
                    });
                    round_events.emit(AgentRawEvent::RoundCompleted {
                        outcome: RoundOutcome::Failed,
                    });
                    return Err(error);
                }
            };
            sink.record_usage(&response.usage);
            if let Err(error) = response.validate() {
                let (usage, partial) = sink.abort(&error.to_string());
                progress.reported_usage |= sink.reported_usage();
                progress.usage.accumulate(&usage);
                limits.account_tokens(
                    &usage,
                    input_estimate.saturating_add(self.count_partial(&model_spec, &partial)),
                );
                progress.partial_response = (!partial.is_empty()).then_some(partial);
                return Err(error.into());
            }
            sink.finish(&response);
            progress.reported_usage |= sink.reported_usage();
            progress.usage.accumulate(&sink.usage());
            limits.account_tokens(
                &response.usage,
                input_estimate
                    .saturating_add(self.counting.output(&model_spec, &response.message).tokens),
            );
            progress.last_model_stop = Some(response.stop_reason.clone());
            request.options.max_output_tokens = original_output_limit;
            if let Err(error) = limits.check() {
                if limits.output_limited() {
                    let (_, partial) = sink.retained();
                    progress.partial_response = (!partial.is_empty()).then_some(partial);
                } else {
                    progress.messages.push(response.message);
                }
                return Err(error);
            }
            drop(sink);
            if let Some(limit) = limits.budget.max_tokens
                && limits.remaining_tokens() == Some(0)
                && response.stop_reason != StopReason::Complete
            {
                let mut snapshot = RunProgress::new(Vec::new());
                limits.snapshot(&mut snapshot);
                limits.stop(BudgetExceeded {
                    resource: BudgetResource::Tokens,
                    limit,
                    used: snapshot.tokens.used,
                    requested: None,
                });
            }
            let finish_reason = Some(response.stop_reason.clone());
            let assistant_message = response.message;
            let tool_calls = assistant_message.tool_calls().cloned().collect::<Vec<_>>();
            progress.messages.push(assistant_message.clone());
            anyhow::ensure!(
                tool_calls.is_empty()
                    || (model_spec.capabilities.tools
                        && request.tool_choice != ModelToolChoice::None),
                "model returned forbidden tool calls"
            );
            if let ModelToolChoice::Named(name) = &request.tool_choice {
                anyhow::ensure!(
                    tool_calls.iter().all(|call| &call.name == name) && !tool_calls.is_empty(),
                    "model did not call the requested tool"
                );
            }
            anyhow::ensure!(
                request.tool_choice != ModelToolChoice::Required || !tool_calls.is_empty(),
                "model did not provide a required tool call"
            );
            context_messages.push(assistant_message.clone());

            if tool_calls.is_empty() {
                if finish_reason != Some(StopReason::Complete) {
                    if budget_clamped && finish_reason == Some(StopReason::MaxTokens) {
                        limits.stop(BudgetExceeded {
                            resource: BudgetResource::Tokens,
                            limit: limits.budget.max_tokens.unwrap(),
                            used: limits
                                .budget
                                .max_tokens
                                .unwrap()
                                .saturating_sub(limits.remaining_tokens().unwrap()),
                            requested: None,
                        });
                        limits.check()?;
                    }
                    return Err(ModelStopped(finish_reason.clone().unwrap()).into());
                }
                limits.check()?;
                progress.stage = RunStage::Verification;
                transition_state(
                    state,
                    &round_events,
                    AgentRunState::VerifyingCompletion { round },
                )?;
                round_events.emit(AgentRawEvent::CompletionVerificationStarted);
                let verification = self.completion_verifier.verify(CompletionContext {
                    workspace: self.workspace_root(),
                    round,
                    candidate: &assistant_message,
                    messages: &progress.messages,
                    usage: &progress.usage,
                });
                let verdict = tokio::select! {
                    biased;
                    cancelled = context.cancelled() => Err(anyhow::Error::from(cancelled)),
                    verdict = verification => verdict,
                }
                .with_context(|| format!("Completion verification failed in agent round {round}"));
                match verdict {
                    Ok(CompletionVerdict::Accepted) => {
                        round_events.emit(AgentRawEvent::CompletionVerificationEnded {
                            outcome: crate::events::CompletionVerificationOutcome::Accepted,
                            feedback: None,
                        });
                        limits.check()?;
                        let more_input = context
                            .control
                            .as_ref()
                            .is_some_and(|control| control.next(true, true).is_some());
                        if more_input {
                            checkpoint(
                                journal,
                                progress.messages.len() - 1,
                                &assistant_message,
                                context,
                            )
                            .await?;
                        }
                        if more_input
                            && apply_inputs(
                                &mut context_messages,
                                progress,
                                context,
                                journal,
                                InputBoundary::AfterCompletion {
                                    round_limit: self.config.max_rounds,
                                },
                                &round_events,
                            )
                            .await?
                        {
                            round_events.emit(AgentRawEvent::RoundCompleted {
                                outcome: RoundOutcome::FinalAnswer,
                            });
                            continue;
                        }
                        round_events.emit(AgentRawEvent::RoundCompleted {
                            outcome: RoundOutcome::FinalAnswer,
                        });
                        return Ok((assistant_message, finish_reason));
                    }
                    Ok(CompletionVerdict::Rejected { feedback }) => {
                        let feedback = bounded_verifier_feedback(feedback);
                        round_events.emit(AgentRawEvent::CompletionVerificationEnded {
                            outcome: crate::events::CompletionVerificationOutcome::Rejected,
                            feedback: Some(feedback.clone().into()),
                        });
                        let feedback = completion_feedback_message(feedback);
                        checkpoint(
                            journal,
                            progress.messages.len() - 1,
                            &assistant_message,
                            context,
                        )
                        .await?;
                        progress.messages.push(feedback.clone());
                        checkpoint(journal, progress.messages.len() - 1, &feedback, context)
                            .await?;
                        context_messages.push(feedback);
                        round_events.emit(AgentRawEvent::RoundCompleted {
                            outcome: RoundOutcome::CompletionRejected,
                        });
                        continue;
                    }
                    Err(error) if error.downcast_ref::<TurnCancelled>().is_some() => {
                        round_events.emit(AgentRawEvent::CompletionVerificationEnded {
                            outcome: crate::events::CompletionVerificationOutcome::Failed,
                            feedback: Some(context.reason()),
                        });
                        round_events.emit(AgentRawEvent::RoundCompleted {
                            outcome: RoundOutcome::Aborted,
                        });
                        return Err(error);
                    }
                    Err(error) => {
                        round_events.emit(AgentRawEvent::CompletionVerificationEnded {
                            outcome: crate::events::CompletionVerificationOutcome::Failed,
                            feedback: Some(format!("{error:#}").into()),
                        });
                        round_events.emit(AgentRawEvent::RoundCompleted {
                            outcome: RoundOutcome::Failed,
                        });
                        return Err(error);
                    }
                }
            }

            progress.stage = RunStage::Tools;
            let model_index = progress.messages.len() - 1;
            checkpoint(journal, model_index, &assistant_message, context).await?;
            limits.reserve_tools(tool_calls.len(), self.dispatcher.max_calls_per_round())?;
            transition_state(
                state,
                &round_events,
                AgentRunState::ExecutingTools {
                    round,
                    count: tool_calls.len(),
                },
            )?;
            tracing::debug!(
                round,
                calls = tool_calls.len(),
                "Dispatching model tool calls"
            );
            let capture = crate::tools::DispatchCapture::new(tool_calls.len());
            let dispatched = self
                .dispatcher
                .dispatch_captured(
                    &tool_calls,
                    &round_events,
                    context,
                    limits,
                    &capture,
                    crate::tools::DispatchJournal {
                        journal,
                        base_index: model_index + 1,
                    },
                )
                .await;
            progress.messages.extend(capture.messages());
            let tool_messages = match dispatched {
                Ok(messages) => messages,
                Err(error) if error.downcast_ref::<TurnCancelled>().is_some() => {
                    round_events.emit(AgentRawEvent::RoundCompleted {
                        outcome: RoundOutcome::Aborted,
                    });
                    return Err(error);
                }
                Err(error) => {
                    round_events.emit(AgentRawEvent::RoundCompleted {
                        outcome: RoundOutcome::Failed,
                    });
                    return Err(error);
                }
            };
            context_messages.extend(tool_messages);
            limits.check()?;
            round_events.emit(AgentRawEvent::RoundCompleted {
                outcome: RoundOutcome::ToolCalls {
                    count: tool_calls.len(),
                },
            });
        }

        Err(BudgetExceeded {
            resource: BudgetResource::Rounds,
            limit: self.config.max_rounds as u64,
            used: progress.rounds as u64,
            requested: None,
        }
        .into())
    }
}

enum InputBoundary {
    BeforeModel,
    AfterCompletion { round_limit: usize },
}
async fn apply_inputs(
    messages: &mut Vec<Message>,
    progress: &mut RunProgress,
    context: &TurnExecutionContext,
    journal: &dyn crate::RunJournal,
    boundary: InputBoundary,
    events: &AgentEventEmitter,
) -> Result<bool> {
    let Some(control) = &context.control else {
        return Ok(false);
    };
    let mut applied = false;
    let follow_up = matches!(boundary, InputBoundary::AfterCompletion { .. });
    for _ in 0..control.max_pending_inputs() {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let Some(input) = control.next(follow_up && !applied, follow_up && !applied) else {
            break;
        };
        if let InputBoundary::AfterCompletion { round_limit } = boundary
            && progress.rounds >= round_limit
        {
            return Err(BudgetExceeded {
                resource: BudgetResource::Rounds,
                limit: round_limit as u64,
                used: progress.rounds as u64,
                requested: Some(1),
            }
            .into());
        }
        let index = progress.messages.len();
        progress.messages.push(input.message.clone());
        tokio::select! {
            biased;
            cancelled=context.cancelled()=>return Err(cancelled.into()),
            result=journal.input(index,&input)=>{
                result.map_err(|error|anyhow::Error::from(RunFailure::from_error(RunStage::Persistence,&error)))?;
            }
        }
        control.applied(input.id);
        messages.push(input.message.clone());
        events.emit(AgentRawEvent::RunInputApplied {
            input: Arc::new(input),
        });
        applied = true;
    }
    Ok(applied)
}

fn classify_stop(error: &anyhow::Error, limits: &RunLimiter, stage: RunStage) -> RunStop {
    if let Some(limit) = limits.violation() {
        RunStop::Budget(limit)
    } else if let Some(limit) = error.downcast_ref::<BudgetExceeded>() {
        RunStop::Budget(limit.clone())
    } else if let Some(cancelled) = error.downcast_ref::<TurnCancelled>() {
        RunStop::Cancelled {
            reason: cancelled.reason().into(),
        }
    } else if let Some(stopped) = error.downcast_ref::<ModelStopped>() {
        RunStop::Model(stopped.0.clone())
    } else {
        RunStop::Failed(
            error
                .downcast_ref::<RunFailure>()
                .cloned()
                .unwrap_or_else(|| RunFailure::from_error(stage, error)),
        )
    }
}

pub(crate) async fn checkpoint(
    journal: &dyn crate::RunJournal,
    index: usize,
    message: &Message,
    context: &TurnExecutionContext,
) -> Result<()> {
    tokio::select! {biased;cancelled=context.cancelled()=>Err(cancelled.into()),result=journal.checkpoint(index,message)=>result.map_err(|error|anyhow::Error::from(RunFailure::from_error(RunStage::Persistence,&error)))}
}

fn ensure_token_capacity(limits: &RunLimiter, input: u64) -> Result<()> {
    if let Some(remaining) = limits.remaining_tokens()
        && remaining <= input
    {
        let limit = limits.budget.max_tokens.unwrap();
        let error = BudgetExceeded {
            resource: BudgetResource::Tokens,
            limit,
            used: limit.saturating_sub(remaining),
            requested: Some(input.saturating_add(1)),
        };
        limits.stop(error.clone());
        return Err(error.into());
    }
    limits.check()
}
fn transition_state(
    state: &mut AgentRunStateMachine,
    events: &AgentEventEmitter,
    next: AgentRunState,
) -> Result<()> {
    let transition = state.transition(next)?;
    events.emit(AgentRawEvent::StateChanged { transition });
    Ok(())
}

fn ensure_valid_compaction(compaction: &ContextCompaction, target_tokens: usize) -> Result<()> {
    if compaction.estimated_tokens_after > target_tokens {
        anyhow::bail!(
            "Context compactor returned an estimated {} tokens, above the {target_tokens}-token target",
            compaction.estimated_tokens_after
        );
    }
    if compaction.estimated_tokens_after >= compaction.estimated_tokens_before {
        anyhow::bail!("Context compactor did not reduce the estimated context size");
    }
    Ok(())
}

/// Host assembly for the reusable loop. An omitted registry is empty.
pub struct TurnExecutorBuilder {
    provider: Arc<dyn ModelProvider>,
    registry: Option<Arc<ToolRegistry>>,
    workspace: PathBuf,
    approval: Arc<dyn ApprovalHandler>,
    completion_verifier: Arc<dyn CompletionVerifier>,
    context_compactor: Option<Arc<dyn ContextCompactor>>,
    dispatcher_config: DispatcherConfig,
    artifact_store: Option<Arc<dyn crate::artifacts::ArtifactStore>>,
    input_context: Option<Arc<dyn crate::InputContextProvider>>,
    execution_config: TurnExecutionConfig,
    counting: crate::tokens::TokenCounting,
    context_policy: Arc<dyn crate::context_policy::ContextPolicy>,
}

impl TurnExecutorBuilder {
    pub fn new<P>(provider: P) -> Self
    where
        P: ModelProvider + 'static,
    {
        Self::from_shared(Arc::new(provider))
    }

    pub fn from_shared(provider: Arc<dyn ModelProvider>) -> Self {
        Self {
            provider,
            registry: None,
            workspace: PathBuf::from("."),
            approval: Arc::new(DenyAllApproval),
            completion_verifier: Arc::new(AcceptAllCompletionVerifier),
            context_compactor: None,
            dispatcher_config: DispatcherConfig::default(),
            artifact_store: None,
            input_context: None,
            execution_config: TurnExecutionConfig::default(),
            counting: crate::tokens::TokenCounting::default(),
            context_policy: Arc::new(crate::context_policy::WindowContextPolicy),
        }
    }

    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = workspace.into();
        self
    }

    pub fn registry(mut self, registry: ToolRegistry) -> Self {
        self.registry = Some(Arc::new(registry));
        self
    }

    pub fn shared_registry(mut self, registry: Arc<ToolRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn approval<A>(mut self, approval: A) -> Self
    where
        A: ApprovalHandler + 'static,
    {
        self.approval = Arc::new(approval);
        self
    }

    pub fn shared_approval(mut self, approval: Arc<dyn ApprovalHandler>) -> Self {
        self.approval = approval;
        self
    }

    pub fn completion_verifier<V>(mut self, verifier: V) -> Self
    where
        V: CompletionVerifier + 'static,
    {
        self.completion_verifier = Arc::new(verifier);
        self
    }

    pub fn shared_completion_verifier(mut self, verifier: Arc<dyn CompletionVerifier>) -> Self {
        self.completion_verifier = verifier;
        self
    }

    pub fn context_compactor<C>(mut self, compactor: C) -> Self
    where
        C: ContextCompactor + 'static,
    {
        self.context_compactor = Some(Arc::new(compactor));
        self
    }

    pub fn shared_context_compactor(mut self, compactor: Arc<dyn ContextCompactor>) -> Self {
        self.context_compactor = Some(compactor);
        self
    }

    pub fn input_context(mut self, provider: Arc<dyn crate::InputContextProvider>) -> Self {
        self.input_context = Some(provider);
        self
    }

    /// Install explicit scoped artifact storage; no storage is created by core defaults.
    pub fn artifact_store(mut self, store: Arc<dyn crate::artifacts::ArtifactStore>) -> Self {
        self.artifact_store = Some(store);
        self
    }

    pub fn token_counter(mut self, counter: Arc<dyn crate::tokens::TokenCounter>) -> Self {
        self.counting = crate::tokens::TokenCounting::new(counter);
        self
    }
    pub fn context_policy(mut self, policy: Arc<dyn crate::context_policy::ContextPolicy>) -> Self {
        self.context_policy = policy;
        self
    }

    pub fn dispatcher_config(mut self, config: DispatcherConfig) -> Self {
        self.dispatcher_config = config;
        self
    }

    pub fn config(mut self, config: TurnExecutionConfig) -> Self {
        self.execution_config = config;
        self
    }

    pub fn build(self) -> Result<TurnExecutor> {
        self.execution_config.events.validate()?;
        let registry = match self.registry {
            Some(registry) => registry,
            None => Arc::new(ToolRegistry::default()),
        };
        let mut dispatcher = ToolDispatcher::new(
            registry,
            self.workspace,
            self.approval,
            self.dispatcher_config,
        )?;
        if let Some(store) = self.artifact_store {
            dispatcher = dispatcher.with_artifact_store(store)?;
        }
        let mut executor = TurnExecutor::new_with_verifier(
            self.provider,
            Arc::new(dispatcher),
            self.completion_verifier,
            self.context_compactor,
            self.input_context,
            self.execution_config,
        );
        executor.counting = self.counting;
        executor.context_policy = self.context_policy;
        Ok(executor)
    }
}

const MAX_VERIFIER_FEEDBACK_BYTES: usize = 16 * 1024;

fn bounded_verifier_feedback(mut feedback: String) -> String {
    if feedback.len() <= MAX_VERIFIER_FEEDBACK_BYTES {
        return feedback;
    }
    let mut boundary = MAX_VERIFIER_FEEDBACK_BYTES;
    while boundary > 0 && !feedback.is_char_boundary(boundary) {
        boundary -= 1;
    }
    feedback.truncate(boundary);
    feedback.push_str("\n[verification feedback truncated]");
    feedback
}

fn completion_feedback_message(feedback: String) -> Message {
    Message::text(
        crate::model::MessageRole::User,
        format!(
            "Automated completion verification rejected the previous answer. Continue working and address this observation before answering again.\n\n{feedback}"
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::*;
    use crate::{
        ToolExecutionOutcome,
        context::{ContextWindowConfig, PruningContextCompactor},
        model::ToolDefinition,
        tools::{
            AllowAllApproval, ApprovalHandler, DispatcherConfig, ExecutableTool, ToolCapability,
            ToolContext, ToolFailure, ToolInvocation, ToolRegistry, ToolSuccess,
        },
    };

    struct MockProvider {
        responses: Mutex<VecDeque<ModelResponse>>,
        requests: Mutex<Vec<ModelRequest>>,
        reasoning_history: bool,
    }

    struct PendingProvider {
        started: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicBool>,
    }

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl ModelProvider for PendingProvider {
        fn model_spec(
            &self,
            model: &str,
        ) -> std::result::Result<crate::model::ModelSpec, crate::model::ProviderError> {
            let mut spec = crate::model::ModelSpec::new("scripted", model);
            spec.capabilities = crate::model::ModelCapabilities {
                tools: true,
                images: true,
                reasoning_history: true,
                temperature: true,
                top_p: true,
                seed: true,
                stop_sequences: true,
                structured_output: true,
            };
            Ok(spec)
        }
        async fn generate(
            &self,
            _request: ModelRequest,
            events: Arc<dyn crate::model::ModelEventSink>,
            _context: crate::TurnExecutionContext,
        ) -> std::result::Result<ModelResponse, crate::model::ProviderError> {
            let result: anyhow::Result<ModelResponse> = async {
                let _marker = DropMarker(Arc::clone(&self.dropped));
                self.started.notify_one();
                std::future::pending().await
            }
            .await;
            match result {
                Ok(response) => {
                    crate::model::emit_response(&response, events.as_ref());
                    Ok(response)
                }
                Err(error) => Err(crate::model::ProviderError::new(
                    "scripted",
                    crate::model::ProviderErrorKind::Other,
                    error.to_string(),
                )),
            }
        }
    }

    impl MockProvider {
        fn new(responses: Vec<ModelResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                reasoning_history: true,
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for MockProvider {
        fn model_spec(
            &self,
            model: &str,
        ) -> std::result::Result<crate::model::ModelSpec, crate::model::ProviderError> {
            let mut spec = crate::model::ModelSpec::new("scripted", model);
            spec.capabilities = crate::model::ModelCapabilities {
                tools: true,
                images: true,
                reasoning_history: self.reasoning_history,
                temperature: true,
                top_p: true,
                seed: true,
                stop_sequences: true,
                structured_output: true,
            };
            Ok(spec)
        }
        async fn generate(
            &self,
            request: ModelRequest,
            events: Arc<dyn crate::model::ModelEventSink>,
            _context: crate::TurnExecutionContext,
        ) -> std::result::Result<ModelResponse, crate::model::ProviderError> {
            let result: anyhow::Result<ModelResponse> = async {
                self.requests.lock().unwrap().push(request);
                self.responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .context("No mock response available")
            }
            .await;
            match result {
                Ok(response) => {
                    crate::model::emit_response(&response, events.as_ref());
                    Ok(response)
                }
                Err(error) => Err(crate::model::ProviderError::new(
                    "scripted",
                    crate::model::ProviderErrorKind::Other,
                    error.to_string(),
                )),
            }
        }
    }

    struct EchoTool;

    struct CommandEchoTool;

    struct PendingCommandTool {
        started: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicBool>,
    }

    struct PendingApproval {
        started: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl ApprovalHandler for PendingApproval {
        async fn approve(&self, _invocation: &ToolInvocation) -> Result<bool> {
            self.started.notify_one();
            std::future::pending().await
        }
    }

    struct RejectOnceVerifier {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl CompletionVerifier for RejectOnceVerifier {
        async fn verify(&self, _context: CompletionContext<'_>) -> Result<CompletionVerdict> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(CompletionVerdict::rejected("tests have not passed"))
            } else {
                Ok(CompletionVerdict::Accepted)
            }
        }
    }

    #[derive(Default)]
    struct RecordingHandler(Mutex<Vec<AgentRawEvent>>);

    impl AgentEventHandler for RecordingHandler {
        fn on_raw_event(&self, event: &crate::events::AgentRawEventEnvelope) {
            self.0.lock().unwrap().push(event.event.clone());
        }
    }

    #[async_trait]
    impl ExecutableTool for EchoTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "echo".to_string(),
                description: Some("Echo a value".to_string()),
                parameters: json!({"type":"object"}),
            }
        }

        fn capability(&self) -> ToolCapability {
            ToolCapability::ReadOnly
        }

        async fn execute(
            &self,
            _context: &ToolContext,
            arguments: Value,
        ) -> std::result::Result<ToolSuccess, ToolFailure> {
            Ok(ToolSuccess {
                artifacts: Vec::new(),
                output: arguments,
                truncated: false,
            })
        }
    }

    #[async_trait]
    impl ExecutableTool for CommandEchoTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "command_echo".to_string(),
                description: Some("Echo after approval".to_string()),
                parameters: json!({"type":"object"}),
            }
        }

        fn capability(&self) -> ToolCapability {
            ToolCapability::Command
        }

        async fn execute(
            &self,
            _context: &ToolContext,
            arguments: Value,
        ) -> std::result::Result<ToolSuccess, ToolFailure> {
            Ok(ToolSuccess {
                artifacts: Vec::new(),
                output: arguments,
                truncated: false,
            })
        }
    }

    #[async_trait]
    impl ExecutableTool for PendingCommandTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "pending_command".to_string(),
                description: Some("Wait forever for cancellation testing".to_string()),
                parameters: json!({"type":"object"}),
            }
        }

        fn capability(&self) -> ToolCapability {
            ToolCapability::Command
        }

        async fn execute(
            &self,
            _context: &ToolContext,
            _arguments: Value,
        ) -> std::result::Result<ToolSuccess, ToolFailure> {
            let _marker = DropMarker(Arc::clone(&self.dropped));
            self.started.notify_one();
            std::future::pending().await
        }
    }

    fn response(value: Value) -> ModelResponse {
        serde_json::from_value(value).unwrap()
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: "deepseek-chat".to_string(),
            messages: vec![Message::text(
                crate::model::MessageRole::User,
                "echo hello".to_string(),
            )],
            options: ModelOptions {
                max_output_tokens: (Some(128)).and_then(|v| u32::try_from(v).ok()),
                ..Default::default()
            },
            tools: Vec::new(),
            tool_choice: ModelToolChoice::Auto,
            stream: true,
        }
    }

    #[tokio::test]
    async fn agent_executes_tool_and_returns_final_answer() {
        let provider = Arc::new(MockProvider::new(vec![
            response(
                json!({"id":"one","model":"deepseek-chat","message":{"role":"assistant","content":[{"type":"tool_call","call":{"id":"call-1","name":"echo","arguments":{"value": "hello"}}}]},"stop_reason":{"reason":"tool_calls"},"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12,"cached_input_tokens":4,"cache_write_tokens":2}}),
            ),
            response(
                json!({"id":"two","model":"deepseek-chat","message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"stop_reason":{"reason":"complete"},"usage":{"input_tokens":15,"output_tokens":1,"total_tokens":16,"cached_input_tokens":10,"cache_write_tokens":null}}),
            ),
        ]));
        let directory = tempdir().unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).unwrap();
        let dispatcher = Arc::new(
            ToolDispatcher::new(
                Arc::new(registry),
                directory.path(),
                Arc::new(AllowAllApproval),
                DispatcherConfig::default(),
            )
            .unwrap(),
        );
        let agent = TurnExecutor::new(provider.clone(), dispatcher, TurnExecutionConfig::default());

        let result = agent.run(request()).await.unwrap();

        assert_eq!(result.final_message.text_content().as_str(), "done");
        assert_eq!(result.rounds, 2);
        assert_eq!(result.usage.total_tokens, Some(28));
        assert_eq!(result.usage.cached_input_tokens, Some(14));
        assert_eq!(
            result
                .usage
                .input_tokens
                .zip(result.usage.cached_input_tokens)
                .map(|(input, cached)| input - cached),
            Some(11)
        );
        assert_eq!(result.usage.cache_write_tokens, None);
        assert_eq!(result.messages.len(), 4);
        assert_eq!(
            result.messages[2].tool_result().map(|(id, _, _)| id),
            Some("call-1")
        );

        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].stream);
        assert_eq!(requests[0].tools.len(), 1);
        assert_eq!(requests[1].messages.len(), 3);
    }

    #[tokio::test]
    async fn agent_stops_at_round_limit() {
        let tool_response = || {
            response(
                json!({"id":"loop","model":"deepseek-chat","message":{"role":"assistant","content":[{"type":"tool_call","call":{"id":"call-loop","name":"echo","arguments":{}}}]},"stop_reason":{"reason":"tool_calls"},"usage":{"input_tokens":null,"output_tokens":null,"total_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null}}),
            )
        };
        let provider = Arc::new(MockProvider::new(vec![tool_response(), tool_response()]));
        let directory = tempdir().unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).unwrap();
        let dispatcher = Arc::new(
            ToolDispatcher::new(
                Arc::new(registry),
                directory.path(),
                Arc::new(AllowAllApproval),
                DispatcherConfig::default(),
            )
            .unwrap(),
        );
        let agent = TurnExecutor::new(
            provider,
            dispatcher,
            TurnExecutionConfig {
                max_rounds: 2,
                stream: true,
                ..Default::default()
            },
        );

        let error = agent.run(request()).await.unwrap_err();
        assert!(error.to_string().contains("exceeded maximum"));
    }

    #[tokio::test]
    async fn agent_feeds_tool_error_back_to_model() {
        let provider = Arc::new(MockProvider::new(vec![
            response(
                json!({"id":"one","model":"deepseek-chat","message":{"role":"assistant","content":[{"type":"tool_call","call":{"id":"bad-call","name":"missing","arguments":{}}}]},"stop_reason":{"reason":"tool_calls"},"usage":{"input_tokens":null,"output_tokens":null,"total_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null}}),
            ),
            response(
                json!({"id":"two","model":"deepseek-chat","message":{"role":"assistant","content":[{"type":"text","text":"recovered"}]},"stop_reason":{"reason":"complete"},"usage":{"input_tokens":null,"output_tokens":null,"total_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null}}),
            ),
        ]));
        let directory = tempdir().unwrap();
        let dispatcher = Arc::new(
            ToolDispatcher::new(
                Arc::new(ToolRegistry::default()),
                directory.path(),
                Arc::new(AllowAllApproval),
                DispatcherConfig::default(),
            )
            .unwrap(),
        );
        let agent = TurnExecutor::new(provider.clone(), dispatcher, TurnExecutionConfig::default());

        let result = agent.run(request()).await.unwrap();

        assert_eq!(result.final_message.text_content().as_str(), "recovered");
        let requests = provider.requests.lock().unwrap();
        let tool_result: Value = requests[1].messages[2].tool_result().unwrap().1.clone();
        assert_eq!(tool_result["error"]["code"], "unknown_tool");
    }

    #[tokio::test]
    async fn agent_continues_after_completion_rejection() {
        let provider = Arc::new(MockProvider::new(vec![
            response(
                json!({"id":"one","model":"deepseek-chat","message":{"role":"assistant","content":[{"type":"text","text":"done too early"}]},"stop_reason":{"reason":"complete"},"usage":{"input_tokens":null,"output_tokens":null,"total_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null}}),
            ),
            response(
                json!({"id":"two","model":"deepseek-chat","message":{"role":"assistant","content":[{"type":"text","text":"verified"}]},"stop_reason":{"reason":"complete"},"usage":{"input_tokens":null,"output_tokens":null,"total_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null}}),
            ),
        ]));
        let directory = tempdir().unwrap();
        let handler = Arc::new(RecordingHandler::default());
        let agent = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(directory.path())
            .approval(AllowAllApproval)
            .completion_verifier(RejectOnceVerifier {
                calls: AtomicUsize::new(0),
            })
            .build()
            .unwrap();

        let result = agent
            .run_with_handler(request(), handler.clone())
            .await
            .unwrap();

        assert_eq!(result.rounds, 2);
        assert_eq!(result.final_message.text_content().as_str(), "verified");
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].messages.len(), 3);
        assert_eq!(
            requests[1].messages[2].role,
            crate::model::MessageRole::User
        );
        assert!(
            requests[1].messages[2]
                .text_content()
                .as_str()
                .contains("tests have not passed")
        );
        drop(requests);

        let events = handler.0.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::CompletionVerificationEnded {
                outcome: crate::events::CompletionVerificationOutcome::Rejected,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::CompletionVerificationEnded {
                outcome: crate::events::CompletionVerificationOutcome::Accepted,
                ..
            }
        )));
        let states = events
            .iter()
            .filter_map(|event| match event {
                AgentRawEvent::StateChanged { transition } => Some(transition.current),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            states,
            vec![
                AgentRunState::Preparing,
                AgentRunState::AwaitingModel { round: 1 },
                AgentRunState::VerifyingCompletion { round: 1 },
                AgentRunState::AwaitingModel { round: 2 },
                AgentRunState::VerifyingCompletion { round: 2 },
                AgentRunState::Completed { rounds: 2 },
            ]
        );
    }

    #[tokio::test]
    async fn turn_executor_compacts_model_context_but_preserves_transcript() {
        let provider = Arc::new(MockProvider::new(vec![response(
            json!({"id":"compact","model":"deepseek-chat","message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"stop_reason":{"reason":"complete"},"usage":{"input_tokens":null,"output_tokens":null,"total_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null}}),
        )]));
        let directory = tempdir().unwrap();
        let handler = Arc::new(RecordingHandler::default());
        let compactor = PruningContextCompactor::new(ContextWindowConfig {
            max_context_tokens: 180,
            reserved_output_tokens: 40,
            preserve_recent_tokens: 30,
        })
        .unwrap();
        let executor = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(directory.path())
            .registry(ToolRegistry::default())
            .context_compactor(compactor)
            .build()
            .unwrap();
        let mut request = request();
        request.options.max_output_tokens = Some(40);
        let old_context = "old context ".repeat(100);
        request.messages = vec![
            Message::text(crate::model::MessageRole::System, "system".to_string()),
            Message::text(crate::model::MessageRole::User, old_context.clone()),
            Message::text(
                crate::model::MessageRole::User,
                "latest request".to_string(),
            ),
        ];

        let result = executor
            .run_with_handler(request, handler.clone())
            .await
            .unwrap();

        assert_eq!(result.messages.len(), 4);
        assert_eq!(
            result.messages[1].text_content().as_str(),
            old_context.as_str()
        );
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].messages.len(), 3);
        assert!(requests[0].messages.iter().any(|message| {
            message
                .text_content()
                .contains("Context compaction removed")
        }));
        assert!(
            !requests[0]
                .messages
                .iter()
                .any(|message| message.text_content().as_str() == old_context.as_str())
        );
        drop(requests);

        let events = handler.0.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::ContextCompactionCompleted {
                removed_messages: 1,
                ..
            }
        )));
        let states = events
            .iter()
            .filter_map(|event| match event {
                AgentRawEvent::StateChanged { transition } => Some(transition.current),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            states,
            vec![
                AgentRunState::Preparing,
                AgentRunState::AwaitingModel { round: 1 },
                AgentRunState::Compacting { round: 1 },
                AgentRunState::AwaitingModel { round: 1 },
                AgentRunState::VerifyingCompletion { round: 1 },
                AgentRunState::Completed { rounds: 1 },
            ]
        );
    }

    #[tokio::test]
    async fn cancellation_aborts_state_and_drops_inflight_model_request() {
        let started = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let directory = tempdir().unwrap();
        let handler = Arc::new(RecordingHandler::default());
        let executor = TurnExecutor::builder(PendingProvider {
            started: Arc::clone(&started),
            dropped: Arc::clone(&dropped),
        })
        .workspace(directory.path())
        .registry(ToolRegistry::default())
        .build()
        .unwrap();
        let context = TurnExecutionContext::new();
        let cancellation = context.clone();

        let run = tokio::spawn(async move {
            executor
                .run_with_context(request(), handler.clone(), context)
                .await
                .map_err(|error| (error, handler))
        });
        started.notified().await;
        cancellation.cancel("user pressed escape");
        let (error, handler) = run.await.unwrap().unwrap_err();

        assert_eq!(
            error.downcast_ref::<TurnCancelled>().unwrap().reason(),
            "user pressed escape"
        );
        assert!(dropped.load(Ordering::SeqCst));
        let events = handler.0.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::RoundCompleted {
                outcome: RoundOutcome::Aborted
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::StateChanged {
                transition: crate::state::AgentStateTransition {
                    current: AgentRunState::Aborted { round: Some(1) },
                    ..
                }
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::AgentAborted { reason } if reason.as_ref() == "user pressed escape"
        )));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentRawEvent::AgentFailed { .. }))
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_pending_tool_approval_and_closes_events() {
        let provider = Arc::new(MockProvider::new(vec![response(
            json!({"id":"approval","model":"deepseek-chat","message":{"role":"assistant","content":[{"type":"tool_call","call":{"id":"call-approval","name":"command_echo","arguments":{}}}]},"stop_reason":{"reason":"tool_calls"},"usage":{"input_tokens":null,"output_tokens":null,"total_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null}}),
        )]));
        let approval_started = Arc::new(tokio::sync::Notify::new());
        let directory = tempdir().unwrap();
        let handler = Arc::new(RecordingHandler::default());
        let mut registry = ToolRegistry::default();
        registry.register(CommandEchoTool).unwrap();
        let executor = TurnExecutor::builder_from_shared(provider)
            .workspace(directory.path())
            .registry(registry)
            .approval(PendingApproval {
                started: Arc::clone(&approval_started),
            })
            .build()
            .unwrap();
        let context = TurnExecutionContext::new();
        let cancellation = context.clone();

        let run = tokio::spawn(async move {
            executor
                .run_with_context(request(), handler.clone(), context)
                .await
                .map_err(|error| (error, handler))
        });
        tokio::time::timeout(Duration::from_secs(2), approval_started.notified())
            .await
            .expect("approval was not requested");
        cancellation.cancel("cancel approval");
        let (error, handler) = run.await.unwrap().unwrap_err();

        assert!(error.downcast_ref::<TurnCancelled>().is_some());
        let events = handler.0.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::ToolApprovalResolved { call_id, approved: false, .. }
                if call_id.as_ref() == "call-approval"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::ToolExecutionEnded { call_id, outcome, .. }
                if call_id.as_ref() == "call-approval"
                    && matches!(outcome.as_ref(), ToolExecutionOutcome::Cancelled)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::RoundCompleted {
                outcome: RoundOutcome::Aborted
            }
        )));
    }

    #[tokio::test]
    async fn cancellation_drops_inflight_tool_execution() {
        let provider = Arc::new(MockProvider::new(vec![response(
            json!({"id":"tool","model":"deepseek-chat","message":{"role":"assistant","content":[{"type":"tool_call","call":{"id":"call-tool","name":"pending_command","arguments":{}}}]},"stop_reason":{"reason":"tool_calls"},"usage":{"input_tokens":null,"output_tokens":null,"total_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null}}),
        )]));
        let tool_started = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let directory = tempdir().unwrap();
        let handler = Arc::new(RecordingHandler::default());
        let mut registry = ToolRegistry::default();
        registry
            .register(PendingCommandTool {
                started: Arc::clone(&tool_started),
                dropped: Arc::clone(&dropped),
            })
            .unwrap();
        let executor = TurnExecutor::builder_from_shared(provider)
            .workspace(directory.path())
            .registry(registry)
            .approval(AllowAllApproval)
            .build()
            .unwrap();
        let context = TurnExecutionContext::new();
        let cancellation = context.clone();

        let run = tokio::spawn(async move {
            executor
                .run_with_context(request(), handler.clone(), context)
                .await
                .map_err(|error| (error, handler))
        });
        tokio::time::timeout(Duration::from_secs(2), tool_started.notified())
            .await
            .expect("tool execution did not start");
        cancellation.cancel("stop tool");
        let (error, handler) = run.await.unwrap().unwrap_err();

        assert!(error.downcast_ref::<TurnCancelled>().is_some());
        assert!(dropped.load(Ordering::SeqCst));
        let events = handler.0.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentRawEvent::ToolExecutionEnded { call_id, outcome, .. }
                if call_id.as_ref() == "call-tool"
                    && matches!(outcome.as_ref(), ToolExecutionOutcome::Cancelled)
        )));
    }
    #[tokio::test]
    async fn reasoning_is_preserved_in_transcript_but_projected_by_model_capability() {
        use crate::model::{ContentBlock, MessageRole, ModelToolCall};
        let first = ModelResponse {
            id: None,
            model: "deepseek-chat".into(),
            message: Message {
                role: MessageRole::Assistant,
                content: vec![
                    ContentBlock::Reasoning {
                        text: "private analysis".into(),
                    },
                    ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "c".into(),
                            name: "echo".into(),
                            arguments: json!({}),
                        },
                    },
                ],
            },
            stop_reason: StopReason::ToolCalls,
            usage: ModelUsage::default(),
        };
        let mut second = first.clone();
        second.message = Message::text(MessageRole::Assistant, "done");
        second.stop_reason = StopReason::Complete;
        let mut provider = MockProvider::new(vec![first, second]);
        provider.reasoning_history = false;
        let provider = Arc::new(provider);
        let workspace = tempdir().unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).unwrap();
        let executor = TurnExecutor::builder_from_shared(provider.clone())
            .workspace(workspace.path())
            .registry(registry)
            .build()
            .unwrap();
        let result = executor.run(request()).await.unwrap();
        assert_eq!(result.usage.total_tokens, None);
        assert!(
            result.messages[1]
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Reasoning { .. }))
        );
        let requests = provider.requests.lock().unwrap();
        assert!(requests[1].messages.iter().all(|m| {
            m.content
                .iter()
                .all(|b| !matches!(b, ContentBlock::Reasoning { .. }))
        }));
        assert!(requests[1].messages[1].tool_calls().next().is_some());
    }
}
