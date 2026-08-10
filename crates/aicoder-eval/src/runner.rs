use std::{path::Path, sync::Arc, time::Instant};

use aicoder_core::{Agent, AgentRunResult, events::AgentEventHandler, types::FinishReason};
use anyhow::{Context, Result, ensure};

use crate::{
    AgentTrace, AgentTraceRecorder, EvalCase, EvalReport, EvalRunOutcome, EvalRunSummary,
    EvalSuiteReport, EvaluationContext, EvaluationResult, Evaluator, UsageSummary, WorkspaceDiff,
    WorkspaceSnapshot,
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

    pub async fn run<F>(&self, case: &EvalCase, agent_factory: &F) -> Result<EvalReport>
    where
        F: Fn(&Path) -> Result<Agent>,
    {
        let temporary = tempfile::Builder::new()
            .prefix("aicoder-eval-")
            .tempdir()
            .context("Failed to create temporary evaluation workspace")?;
        prepare_fixture(&case.fixture, temporary.path())?;
        let before = WorkspaceSnapshot::capture(temporary.path())?;
        let agent = agent_factory(temporary.path()).context("Failed to build evaluation agent")?;
        let recorder = AgentTraceRecorder::shared();
        let event_handler: Arc<dyn AgentEventHandler> = recorder.clone();
        let started_at = Instant::now();
        let execution = agent
            .run_with_handler(case.request.clone(), event_handler)
            .await;
        let duration = started_at.elapsed();
        let trace = recorder.trace();
        let after = WorkspaceSnapshot::capture(temporary.path())?;
        let workspace_diff = WorkspaceDiff::between(&before, &after);
        let workspace_patch = git_patch(temporary.path())?;
        let (result, run_error) = match execution {
            Ok(result) => (Some(result), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let trajectory = trace.summary();
        let outcome = infer_outcome(result.as_ref(), run_error.as_deref());
        let metadata = case.metadata();
        let context = EvaluationContext {
            case: &metadata,
            workspace: temporary.path(),
            result: result.as_ref(),
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
        let run = build_run_summary(result.as_ref(), run_error, outcome, duration, &trace);
        Ok(EvalReport {
            case: metadata,
            run,
            trajectory,
            workspace_diff,
            workspace_patch,
            evaluations,
            verdict,
            trace,
        })
    }

    pub async fn run_many<F>(
        &self,
        case: &EvalCase,
        repetitions: usize,
        agent_factory: &F,
    ) -> Result<EvalSuiteReport>
    where
        F: Fn(&Path) -> Result<Agent>,
    {
        ensure!(repetitions > 0, "Evaluation repetitions must be positive");
        let mut runs = Vec::with_capacity(repetitions);
        for _ in 0..repetitions {
            runs.push(self.run(case, agent_factory).await?);
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

fn infer_outcome(result: Option<&AgentRunResult>, run_error: Option<&str>) -> EvalRunOutcome {
    if run_error.is_some() || result.is_none() {
        EvalRunOutcome::Failed
    } else if result.is_some_and(|result| {
        result.finish_reason == Some(FinishReason::Stop)
            && result
                .final_message
                .content
                .as_deref()
                .is_some_and(|content| !content.trim().is_empty())
    }) {
        EvalRunOutcome::Completed
    } else {
        EvalRunOutcome::Incomplete
    }
}

fn build_run_summary(
    result: Option<&AgentRunResult>,
    error: Option<String>,
    outcome: EvalRunOutcome,
    duration: std::time::Duration,
    trace: &AgentTrace,
) -> EvalRunSummary {
    let provider_reported = trace.events.iter().any(|event| {
        matches!(
            event.envelope.event,
            aicoder_core::AgentRawEvent::UsageUpdated { .. }
        )
    });
    let traced_usage = if result.is_none() {
        let mut usage = aicoder_core::types::Usage::default();
        for event in &trace.events {
            if let aicoder_core::AgentRawEvent::UsageUpdated { usage: update } =
                &event.envelope.event
            {
                usage.accumulate(update);
            }
        }
        Some(usage)
    } else {
        None
    };
    let usage = result.map(|result| &result.usage).or(traced_usage.as_ref());
    EvalRunSummary {
        outcome,
        duration_ms: duration.as_millis().try_into().unwrap_or(u64::MAX),
        rounds: result.map_or_else(|| trace.summary().rounds, |result| result.rounds),
        final_content: result.and_then(|result| result.final_message.content.clone()),
        error,
        usage: UsageSummary {
            provider_reported,
            prompt_tokens: usage.map_or(0, |usage| usage.prompt_tokens),
            completion_tokens: usage.map_or(0, |usage| usage.completion_tokens),
            total_tokens: usage.map_or(0, |usage| usage.total_tokens),
            cached_tokens: usage.map_or(0, |usage| usage.cached_tokens()),
        },
    }
}
