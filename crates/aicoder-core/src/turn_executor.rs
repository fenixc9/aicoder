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
    tools::{
        ApprovalHandler, DenyAllApproval, DispatcherConfig, ToolDispatcher, ToolRegistry,
        default_registry,
    },
};

#[derive(Debug, Clone)]
pub struct TurnExecutionConfig {
    pub max_rounds: usize,
    pub stream: bool,
    pub budget: RunBudget,
}

impl Default for TurnExecutionConfig {
    fn default() -> Self {
        Self {
            max_rounds: 8,
            stream: true,
            budget: RunBudget::default(),
        }
    }
}

pub struct TurnExecutor {
    provider: Arc<dyn ModelProvider>,
    dispatcher: Arc<ToolDispatcher>,
    completion_verifier: Arc<dyn CompletionVerifier>,
    context_compactor: Option<Arc<dyn ContextCompactor>>,
    config: TurnExecutionConfig,
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
            config,
        )
    }

    fn new_with_verifier(
        provider: Arc<dyn ModelProvider>,
        dispatcher: Arc<ToolDispatcher>,
        completion_verifier: Arc<dyn CompletionVerifier>,
        context_compactor: Option<Arc<dyn ContextCompactor>>,
        config: TurnExecutionConfig,
    ) -> Self {
        Self {
            provider,
            dispatcher,
            completion_verifier,
            context_compactor,
            config,
        }
    }

    pub fn workspace_root(&self) -> &std::path::Path {
        self.dispatcher.workspace_root()
    }

    pub async fn run(&self, request: ModelRequest) -> Result<TurnExecutionResult> {
        self.run_with_context(request, Arc::new(()), TurnExecutionContext::new())
            .await
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
        let context = context.child();
        let limits = Arc::new(RunLimiter::new(self.config.budget.clone(), context.clone()));
        let mut progress = RunProgress::new(request.messages.clone());
        let events = AgentEventEmitter::new(handler);
        events.emit(AgentRawEvent::AgentStarted {
            model: request.model.clone().into(),
        });
        let mut state = AgentRunStateMachine::new();
        let mut execution = match transition_state(&mut state, &events, AgentRunState::Preparing) {
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
                    request,
                    &events,
                    &mut state,
                    &context,
                    &mut progress,
                    &limits,
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
        let mut stop = execution
            .as_ref()
            .err()
            .map(|error| classify_stop(error, &limits, progress.stage));
        let round = (progress.rounds > 0).then_some(progress.rounds);
        let terminal = match &stop {
            None => AgentRunState::Completed {
                rounds: progress.rounds,
            },
            Some(RunStop::Cancelled { .. }) => AgentRunState::Aborted { round },
            Some(RunStop::Budget(_) | RunStop::Model(_)) => AgentRunState::Stopped { round },
            Some(RunStop::Failed(_)) => AgentRunState::Failed { round },
        };
        if let Err(error) = transition_state(&mut state, &events, terminal) {
            stop = Some(RunStop::Failed(RunFailure::from_error(
                progress.stage,
                &error,
            )));
        }
        let outcome = match (execution, stop) {
            (Ok((final_message, finish_reason)), None) => {
                events.emit(AgentRawEvent::AgentCompleted {
                    rounds: progress.rounds,
                    usage: progress.usage.clone().into(),
                });
                RunOutcome::Completed(TurnExecutionResult {
                    final_message,
                    finish_reason,
                    progress,
                })
            }
            (result, stop) => {
                let stop = stop.unwrap_or_else(|| {
                    RunStop::Failed(RunFailure::from_error(progress.stage, &result.unwrap_err()))
                });
                match &stop {
                    RunStop::Cancelled { reason } => events.emit(AgentRawEvent::AgentAborted {
                        reason: reason.clone().into(),
                    }),
                    RunStop::Budget(_) | RunStop::Model(_) => {
                        events.emit(AgentRawEvent::AgentStopped {
                            stop: Arc::new(stop.clone()),
                        })
                    }
                    RunStop::Failed(_) => events.emit(AgentRawEvent::AgentFailed {
                        stage: AgentStage::Turn,
                        message: stop.to_string().into(),
                    }),
                }
                RunOutcome::Interrupted(InterruptedTurn { stop, progress })
            }
        };
        context.cancel("run finished");
        events.shutdown().await;
        outcome
    }

    async fn run_inner(
        &self,
        mut request: ModelRequest,
        events: &AgentEventEmitter,
        state: &mut AgentRunStateMachine,
        context: &TurnExecutionContext,
        progress: &mut RunProgress,
        limits: &Arc<RunLimiter>,
    ) -> Result<(Message, Option<StopReason>)> {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        limits.check()?;

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

        let mut context_messages = progress.messages.clone();

        for round in 1..=self.config.max_rounds {
            limits.check()?;
            progress.rounds = round;
            let round_events = events.for_round(round);
            round_events.emit(AgentRawEvent::RoundStarted);
            transition_state(state, &round_events, AgentRunState::AwaitingModel { round })?;
            if let Some(compactor) = &self.context_compactor {
                let input = ContextCompactionInput {
                    model: &request.model,
                    messages: &context_messages,
                    tools: Some(request.tools.as_slice()),
                    max_output_tokens: request.options.max_output_tokens,
                };
                let estimated_tokens = compactor.estimate_tokens(input);
                let target_tokens = compactor.target_tokens(input);
                if estimated_tokens > target_tokens {
                    transition_state(state, &round_events, AgentRunState::Compacting { round })?;
                    round_events.emit(AgentRawEvent::ContextCompactionStarted {
                        strategy: compactor.name().into(),
                        estimated_tokens,
                        target_tokens,
                    });
                    progress.stage = RunStage::Compaction;
                    if compactor.uses_model() {
                        ensure_token_capacity(limits, estimated_tokens as u64)?;
                    }
                    let compacted = match tokio::select! {
                        biased;
                        cancelled = context.cancelled() => Err(cancelled.into()),
                        compacted = compactor.compact(input) => compacted.and_then(|compacted| {
                            ensure_valid_compaction(&compacted, target_tokens)?;
                            Ok(compacted)
                        }),
                    } {
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
                    if compacted.usage.total_tokens != Some(0) {
                        round_events.emit(AgentRawEvent::UsageUpdated {
                            usage: compacted.usage.clone().into(),
                        });
                    }
                    progress.reported_usage |= compacted.usage != crate::model::ModelUsage::zero()
                        && crate::run::usage_reported(&compacted.usage);
                    progress.usage.accumulate(&compacted.usage);
                    limits.account_tokens(
                        &compacted.usage,
                        (compacted.estimated_tokens_before as u64)
                            .saturating_add(compacted.estimated_tokens_after as u64),
                    );
                    context_messages = compacted.messages;
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
            request.validate(&model_spec).map_err(anyhow::Error::from)?;
            progress.stage = RunStage::Model;
            let input_estimate =
                crate::context::estimate_context_tokens(&request.messages, Some(&request.tools))
                    as u64;
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
            let response = match tokio::select! {
                biased;
                cancelled = context.cancelled() => Err(anyhow::Error::from(cancelled)),
                result = self.provider.generate(request.clone(), sink.clone(), context.clone()) =>
                    result.map_err(|error| if error.kind == crate::model::ProviderErrorKind::Cancelled {
                        context.cancel(error.message);
                        anyhow::Error::from(context.error())
                    } else { anyhow::Error::from(error) }),
            }.with_context(|| format!("Model request failed in agent round {round}"))
            {
                Ok(response) => response,
                Err(error) if error.downcast_ref::<TurnCancelled>().is_some() => {
                    let (usage,partial)=sink.abort(&context.reason());
                    progress.reported_usage|=crate::run::usage_reported(&usage);progress.usage.accumulate(&usage);limits.account_tokens(&usage,input_estimate.saturating_add(estimate_partial(&partial)));
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
                    progress.reported_usage|=crate::run::usage_reported(&usage);progress.usage.accumulate(&usage);limits.account_tokens(&usage,input_estimate.saturating_add(estimate_partial(&partial)));
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
                    input_estimate.saturating_add(estimate_partial(&partial)),
                );
                progress.partial_response = (!partial.is_empty()).then_some(partial);
                return Err(error.into());
            }
            sink.finish(&response);
            progress.reported_usage |= sink.reported_usage();
            progress.usage.accumulate(&sink.usage());
            limits.account_tokens(
                &response.usage,
                input_estimate.saturating_add(estimate_message(&response.message)),
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
                        round_events.emit(AgentRawEvent::RoundCompleted {
                            outcome: RoundOutcome::FinalAnswer,
                        });
                        limits.check()?;
                        return Ok((assistant_message, finish_reason));
                    }
                    Ok(CompletionVerdict::Rejected { feedback }) => {
                        let feedback = bounded_verifier_feedback(feedback);
                        round_events.emit(AgentRawEvent::CompletionVerificationEnded {
                            outcome: crate::events::CompletionVerificationOutcome::Rejected,
                            feedback: Some(feedback.clone().into()),
                        });
                        let feedback = completion_feedback_message(feedback);
                        progress.messages.push(feedback.clone());
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
                .dispatch_captured(&tool_calls, &round_events, context, limits, &capture)
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
        RunStop::Failed(RunFailure::from_error(stage, error))
    }
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
fn estimate_text(text: &str) -> u64 {
    let ascii = text.chars().filter(char::is_ascii).count() as u64;
    ascii
        .div_ceil(4)
        .saturating_add((text.chars().count() as u64 - ascii).saturating_mul(2))
}
fn estimate_message(message: &Message) -> u64 {
    message
        .content
        .iter()
        .map(|block| match block {
            crate::model::ContentBlock::Text { text }
            | crate::model::ContentBlock::Reasoning { text } => estimate_text(text),
            other => estimate_text(&serde_json::to_string(other).unwrap_or_default()),
        })
        .sum()
}
fn estimate_partial(partial: &crate::run::PartialModelResponse) -> u64 {
    estimate_text(&partial.text)
        .saturating_add(estimate_text(&partial.reasoning))
        .saturating_add(
            partial
                .tool_calls
                .iter()
                .map(|call| {
                    estimate_text(&call.arguments).saturating_add(estimate_text(&call.name))
                })
                .sum::<u64>(),
        )
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

/// Convenience assembly for applications that want the built-in coding tools.
pub struct TurnExecutorBuilder {
    provider: Arc<dyn ModelProvider>,
    registry: Option<Arc<ToolRegistry>>,
    workspace: PathBuf,
    approval: Arc<dyn ApprovalHandler>,
    completion_verifier: Arc<dyn CompletionVerifier>,
    context_compactor: Option<Arc<dyn ContextCompactor>>,
    dispatcher_config: DispatcherConfig,
    execution_config: TurnExecutionConfig,
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
            execution_config: TurnExecutionConfig::default(),
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

    pub fn dispatcher_config(mut self, config: DispatcherConfig) -> Self {
        self.dispatcher_config = config;
        self
    }

    pub fn config(mut self, config: TurnExecutionConfig) -> Self {
        self.execution_config = config;
        self
    }

    pub fn build(self) -> Result<TurnExecutor> {
        let registry = match self.registry {
            Some(registry) => registry,
            None => Arc::new(default_registry()?),
        };
        let dispatcher = ToolDispatcher::new(
            registry,
            self.workspace,
            self.approval,
            self.dispatcher_config,
        )?;
        Ok(TurnExecutor::new_with_verifier(
            self.provider,
            Arc::new(dispatcher),
            self.completion_verifier,
            self.context_compactor,
            self.execution_config,
        ))
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
            AgentRawEvent::ToolApprovalResolved { call_id, approved: false }
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
