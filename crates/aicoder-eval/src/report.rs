use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    AgentTrace, EvalCaseMetadata, EvalRunOutcome, EvalVerdict, EvaluationResult, WorkspaceDiff,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TrajectorySummary {
    pub rounds: usize,
    pub final_answer_rounds: usize,
    pub state_transitions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_state: Option<String>,
    pub model_requests: usize,
    pub model_retries: usize,
    pub completion_candidates: usize,
    pub completion_rejections: usize,
    pub completion_verifier_failures: usize,
    pub context_compactions: usize,
    pub context_compaction_failures: usize,
    pub compacted_messages: usize,
    pub tool_calls: usize,
    pub successful_tool_calls: usize,
    pub failed_tool_calls: usize,
    pub timed_out_tool_calls: usize,
    pub approval_denials: usize,
    pub truncated_tool_outputs: usize,
    pub agent_failures: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageSummary {
    pub provider_reported: bool,
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
    pub cached_tokens: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalRunSummary {
    pub outcome: EvalRunOutcome,
    pub duration_ms: u64,
    pub rounds: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub usage: UsageSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub case: EvalCaseMetadata,
    pub run: EvalRunSummary,
    pub trajectory: TrajectorySummary,
    pub workspace_diff: WorkspaceDiff,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_patch: Option<String>,
    pub evaluations: Vec<EvaluationResult>,
    pub verdict: EvalVerdict,
    /// In-memory raw trace. Batch runners persist it as a separate artifact.
    #[serde(skip)]
    pub trace: AgentTrace,
}

impl EvalReport {
    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("Failed to encode evaluation report")
    }

    pub fn write_json(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        fs::write(path, self.to_json_pretty()?)
            .with_context(|| format!("Failed to write evaluation report {}", path.display()))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EvalSuiteSummary {
    pub runs: usize,
    pub passed: usize,
    pub failed: usize,
    pub inconclusive: usize,
    pub evaluator_errors: usize,
    pub pass_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_score: Option<f64>,
    pub mean_duration_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSuiteReport {
    pub case: EvalCaseMetadata,
    pub runs: Vec<EvalReport>,
    pub summary: EvalSuiteSummary,
}

impl EvalSuiteReport {
    pub(crate) fn from_runs(case: EvalCaseMetadata, runs: Vec<EvalReport>) -> Self {
        let mut summary = EvalSuiteSummary {
            runs: runs.len(),
            ..EvalSuiteSummary::default()
        };
        let mut scores = Vec::new();
        let mut total_duration_ms = 0_u128;
        for run in &runs {
            match run.verdict {
                EvalVerdict::Passed => summary.passed += 1,
                EvalVerdict::Failed => summary.failed += 1,
                EvalVerdict::Inconclusive => summary.inconclusive += 1,
                EvalVerdict::EvaluatorError => summary.evaluator_errors += 1,
            }
            total_duration_ms += u128::from(run.run.duration_ms);
            scores.extend(run.evaluations.iter().filter_map(|result| result.score));
        }
        if summary.runs > 0 {
            summary.pass_rate = summary.passed as f64 / summary.runs as f64;
            summary.mean_duration_ms = total_duration_ms as f64 / summary.runs as f64;
        }
        if !scores.is_empty() {
            summary.mean_score = Some(scores.iter().sum::<f64>() / scores.len() as f64);
        }
        Self {
            case,
            runs,
            summary,
        }
    }

    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("Failed to encode evaluation suite report")
    }

    pub fn write_json(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        fs::write(path, self.to_json_pretty()?)
            .with_context(|| format!("Failed to write evaluation suite report {}", path.display()))
    }
}

pub(crate) fn aggregate_verdict(results: &[EvaluationResult]) -> EvalVerdict {
    if results
        .iter()
        .any(|result| result.verdict == EvalVerdict::Failed)
    {
        EvalVerdict::Failed
    } else if results
        .iter()
        .any(|result| result.verdict == EvalVerdict::EvaluatorError)
    {
        EvalVerdict::EvaluatorError
    } else if results
        .iter()
        .any(|result| result.verdict == EvalVerdict::Inconclusive)
    {
        EvalVerdict::Inconclusive
    } else {
        EvalVerdict::Passed
    }
}
