use anyhow::Result;
use async_trait::async_trait;

use crate::{
    EvalFinding, EvalRunOutcome, EvalSeverity, EvalVerdict, EvaluationContext, EvaluationResult,
    Evaluator,
};

/// Grades run completion and configurable trajectory budgets.
#[derive(Debug, Clone)]
pub struct TrajectoryEvaluator {
    name: String,
    require_completed: bool,
    max_rounds: Option<usize>,
    max_tool_calls: Option<usize>,
    max_tool_failures: Option<usize>,
    max_model_retries: Option<usize>,
    max_completion_rejections: Option<usize>,
}

impl TrajectoryEvaluator {
    pub fn new() -> Self {
        Self {
            name: "trajectory".to_string(),
            require_completed: true,
            max_rounds: None,
            max_tool_calls: None,
            max_tool_failures: None,
            max_model_retries: None,
            max_completion_rejections: None,
        }
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn require_completed(mut self, required: bool) -> Self {
        self.require_completed = required;
        self
    }

    pub fn max_rounds(mut self, maximum: usize) -> Self {
        self.max_rounds = Some(maximum);
        self
    }

    pub fn max_tool_calls(mut self, maximum: usize) -> Self {
        self.max_tool_calls = Some(maximum);
        self
    }

    pub fn max_tool_failures(mut self, maximum: usize) -> Self {
        self.max_tool_failures = Some(maximum);
        self
    }

    pub fn max_model_retries(mut self, maximum: usize) -> Self {
        self.max_model_retries = Some(maximum);
        self
    }

    pub fn max_completion_rejections(mut self, maximum: usize) -> Self {
        self.max_completion_rejections = Some(maximum);
        self
    }
}

impl Default for TrajectoryEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Evaluator for TrajectoryEvaluator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn evaluate(&self, context: &EvaluationContext<'_>) -> Result<EvaluationResult> {
        let summary = context.trace.summary();
        let mut findings = Vec::new();
        if self.require_completed && context.outcome != EvalRunOutcome::Completed {
            findings.push(EvalFinding::new(
                EvalSeverity::Error,
                "run_not_completed",
                format!("Agent run ended as {:?}", context.outcome),
            ));
        }
        check_limit(
            &mut findings,
            "round_limit",
            "rounds",
            summary.rounds,
            self.max_rounds,
        );
        check_limit(
            &mut findings,
            "tool_call_limit",
            "tool calls",
            summary.tool_calls,
            self.max_tool_calls,
        );
        check_limit(
            &mut findings,
            "tool_failure_limit",
            "failed tool calls",
            summary.failed_tool_calls,
            self.max_tool_failures,
        );
        check_limit(
            &mut findings,
            "model_retry_limit",
            "model retries",
            summary.model_retries,
            self.max_model_retries,
        );
        check_limit(
            &mut findings,
            "completion_rejection_limit",
            "completion rejections",
            summary.completion_rejections,
            self.max_completion_rejections,
        );
        if summary.completion_verifier_failures > 0 {
            findings.push(EvalFinding::new(
                EvalSeverity::Error,
                "completion_verifier_failed",
                format!(
                    "Completion verifier failed {} time(s)",
                    summary.completion_verifier_failures
                ),
            ));
        }
        if summary.failed_tool_calls > 0 && self.max_tool_failures.is_none() {
            findings.push(EvalFinding::new(
                EvalSeverity::Warning,
                "tool_failures_observed",
                format!(
                    "Trajectory contained {} failed tool call(s)",
                    summary.failed_tool_calls
                ),
            ));
        }
        let failed = findings
            .iter()
            .any(|finding| finding.severity == EvalSeverity::Error);
        Ok(EvaluationResult {
            evaluator: self.name.clone(),
            verdict: if failed {
                EvalVerdict::Failed
            } else {
                EvalVerdict::Passed
            },
            score: Some(if failed { 0.0 } else { 1.0 }),
            findings,
        })
    }
}

fn check_limit(
    findings: &mut Vec<EvalFinding>,
    code: &str,
    label: &str,
    actual: usize,
    maximum: Option<usize>,
) {
    if let Some(maximum) = maximum
        && actual > maximum
    {
        findings.push(EvalFinding::new(
            EvalSeverity::Error,
            code,
            format!("Trajectory used {actual} {label}; maximum is {maximum}"),
        ));
    }
}
