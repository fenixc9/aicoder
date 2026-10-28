use std::{path::Path, sync::Arc, time::Instant};

use aicoder_core::{
    RunOutcome, RunStop, TurnExecutionContext, TurnExecutor, events::AgentEventHandler,
};
use anyhow::{Context, Result, ensure};

use crate::{
    AgentTraceRecorder, EvalCase, EvalReport, EvalRunOutcome, EvalRunSummary, EvalSuiteReport,
    EvaluationContext, EvaluationResult, Evaluator, UsageSummary, WorkspaceDiff, WorkspaceSnapshot,
    report::aggregate_verdict,
    workspace::{git_patch, prepare_fixture},
};

/// Executes isolated evaluation cases and applies post-run evaluators in declaration order.
#[derive(Clone, Default)]
pub struct EvalRunner {
    evaluators: Vec<Arc<dyn Evaluator>>,
}

impl EvalRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn evaluator<E>(mut self, evaluator: E) -> Self
    where
        E: Evaluator + 'static,
    {
        self.evaluators.push(Arc::new(evaluator));
        self
    }

    pub fn shared_evaluator(mut self, evaluator: Arc<dyn Evaluator>) -> Self {
        self.evaluators.push(evaluator);
        self
    }

    pub async fn run<F>(&self, case: &EvalCase, executor_factory: &F) -> Result<EvalReport>
    where
        F: Fn(&Path) -> Result<TurnExecutor>,
    {
        let temporary = tempfile::Builder::new()
            .prefix("aicoder-eval-")
            .tempdir()
            .context("Failed to create temporary evaluation workspace")?;
        prepare_fixture(&case.fixture, temporary.path())?;
        let before = WorkspaceSnapshot::capture(temporary.path())?;
        let executor = executor_factory(temporary.path())
            .context("Failed to build evaluation turn executor")?;
        let recorder = AgentTraceRecorder::shared();
        let event_handler: Arc<dyn AgentEventHandler> = recorder.clone();
        let started_at = Instant::now();
        let execution = executor
            .run_outcome(
                case.request.clone(),
                event_handler,
                TurnExecutionContext::new(),
            )
            .await;
        let duration = started_at.elapsed();
        let trace = recorder.trace();
        let after = WorkspaceSnapshot::capture(temporary.path())?;
        let workspace_diff = WorkspaceDiff::between(&before, &after);
        let workspace_patch = git_patch(temporary.path())?;
        let result = match &execution {
            RunOutcome::Completed(result) => Some(result),
            _ => None,
        };
        let run_error = execution.stop().map(ToString::to_string);
        let trajectory = trace.summary();
        let outcome = match execution.stop() {
            None => EvalRunOutcome::Completed,
            Some(RunStop::Budget(_) | RunStop::Model(_)) => EvalRunOutcome::Incomplete,
            Some(_) => EvalRunOutcome::Failed,
        };
        let metadata = case.metadata();
        let context = EvaluationContext {
            case: &metadata,
            workspace: temporary.path(),
            result,
            run_error: run_error.as_deref(),
            outcome,
            duration,
            trace: &trace,
            workspace_before: &before,
            workspace_after: &after,
            workspace_diff: &workspace_diff,
        };
        let evaluations = self.evaluate_all(&context).await;
        let verdict = aggregate_verdict(&evaluations);
        let progress = execution.progress();
        let usage = &progress.usage;
        let run = EvalRunSummary {
            outcome,
            duration_ms: duration.as_millis().try_into().unwrap_or(u64::MAX),
            rounds: progress.rounds,
            final_content: result.map(|result| result.final_message.text_content()),
            error: run_error,
            stop: execution.stop().cloned(),
            usage: UsageSummary {
                provider_reported: progress.reported_usage,
                prompt_tokens: usage.input_tokens,
                completion_tokens: usage.output_tokens,
                total_tokens: usage.total_tokens,
                cached_tokens: usage.cached_input_tokens,
            },
        };
        let progress = match execution {
            RunOutcome::Completed(result) => result.progress,
            RunOutcome::Interrupted(turn) => turn.progress,
        };
        Ok(EvalReport {
            case: metadata,
            run,
            trajectory,
            workspace_diff,
            workspace_patch,
            evaluations,
            verdict,
            trace,
            progress,
        })
    }

    pub async fn run_many<F>(
        &self,
        case: &EvalCase,
        repetitions: usize,
        executor_factory: &F,
    ) -> Result<EvalSuiteReport>
    where
        F: Fn(&Path) -> Result<TurnExecutor>,
    {
        ensure!(repetitions > 0, "Evaluation repetitions must be positive");
        let mut runs = Vec::with_capacity(repetitions);
        for _ in 0..repetitions {
            runs.push(self.run(case, executor_factory).await?);
        }
        Ok(EvalSuiteReport::from_runs(case.metadata(), runs))
    }

    async fn evaluate_all(&self, context: &EvaluationContext<'_>) -> Vec<EvaluationResult> {
        let mut results = Vec::with_capacity(self.evaluators.len());
        for evaluator in &self.evaluators {
            let name = evaluator.name().to_string();
            match evaluator.evaluate(context).await {
                Ok(result) => results.push(result),
                Err(error) => results.push(EvaluationResult::evaluator_error(
                    name,
                    format!("{error:#}"),
                )),
            }
        }
        results
    }
}
