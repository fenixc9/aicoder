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
    trace_limits: crate::TraceLimits,
}

impl EvalRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn trace_limits(mut self, limits: crate::TraceLimits) -> Result<Self> {
        self.trace_limits = limits.validate()?;
        Ok(self)
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
        self.run_with_context(case, executor_factory, TurnExecutionContext::new())
            .await
    }
    pub async fn run_with_context<F>(
        &self,
        case: &EvalCase,
        executor_factory: &F,
        execution_context: TurnExecutionContext,
    ) -> Result<EvalReport>
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
        let mut provenance =
            crate::EvalProvenance::capture(&executor, &case.request, before.fingerprint())?;
        let journal = crate::provenance::EvaluationJournal::default();
        let recorder = Arc::new(AgentTraceRecorder::with_limits(self.trace_limits)?);
        let event_handler: Arc<dyn AgentEventHandler> = recorder.clone();
        let started_at = Instant::now();
        let execution = executor
            .run_with_journal(
                case.request.clone(),
                event_handler,
                execution_context.clone(),
                &journal,
            )
            .await;
        let duration = started_at.elapsed();
        let trace = recorder.trace();
        let trace_capture = recorder.capture_status();
        provenance.journal = journal.records();
        let captured = WorkspaceSnapshot::capture(temporary.path());
        let (after, mut workspace_error) = match captured {
            Ok(after) => (after, None),
            Err(error) => (
                WorkspaceSnapshot::default(),
                Some(format!("Workspace observation unavailable: {error:#}")),
            ),
        };
        let workspace_diff = if workspace_error.is_none() {
            WorkspaceDiff::between(&before, &after)
        } else {
            WorkspaceDiff::default()
        };
        let workspace_patch = if workspace_error.is_none() {
            let patch = if temporary.path().join(".git").exists() {
                git_patch(temporary.path())
            } else {
                before.unified_patch(&after)
            };
            match patch {
                Ok(patch) => patch,
                Err(error) => {
                    workspace_error = Some(format!("Patch observation unavailable: {error:#}"));
                    None
                }
            }
        } else {
            None
        };
        let workspace_capture_complete = workspace_error.is_none();
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
        let mut evaluations = if let Some(error) = workspace_error {
            vec![EvaluationResult::evaluator_error(
                "workspace_observation",
                error,
            )]
        } else {
            tokio::select! { biased;
                _=execution_context.cancelled()=>vec![EvaluationResult::evaluator_error("grading_cancelled","Grading cancelled; no success was inferred".into())],
                results=self.evaluate_all(&context)=>results,
            }
        };
        let mut validator = aicoder_core::events::EventTraceValidator::new();
        let trace_validation = trace
            .events
            .iter()
            .try_for_each(|record| validator.observe(&record.envelope))
            .and_then(|()| validator.finish());
        if !trace_capture.complete()
            || execution.progress().event_delivery.has_failures()
            || trace_validation.is_err()
        {
            evaluations.push(crate::EvaluationResult::evaluator_error(
                "event_observation",
                trace_validation.err().map_or_else(
                    || "Event delivery was incomplete".into(),
                    |error| error.to_string(),
                ),
            ));
        }
        let verdict = if outcome == EvalRunOutcome::Failed {
            crate::EvalVerdict::Failed
        } else if evaluations.is_empty() {
            crate::EvalVerdict::Inconclusive
        } else {
            let graded = aggregate_verdict(&evaluations);
            if outcome != EvalRunOutcome::Completed && graded == crate::EvalVerdict::Passed {
                crate::EvalVerdict::Inconclusive
            } else {
                graded
            }
        };
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
            provenance: Some(provenance),
            trace_capture,
            workspace_capture_complete,
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
