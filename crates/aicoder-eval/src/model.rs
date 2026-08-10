use std::{path::PathBuf, time::Duration};

use aicoder_core::{AgentRunResult, types::ChatCompletionRequest};
use serde::{Deserialize, Serialize};

use crate::{AgentTrace, WorkspaceDiff, WorkspaceSnapshot};

/// Source used to prepare a fresh workspace for each evaluation run.
#[derive(Debug, Clone)]
pub enum WorkspaceFixture {
    Empty,
    CopyFrom(PathBuf),
    GitCheckout {
        repository: String,
        base_commit: String,
    },
}

impl WorkspaceFixture {
    pub fn git_checkout(repository: impl Into<String>, base_commit: impl Into<String>) -> Self {
        Self::GitCheckout {
            repository: repository.into(),
            base_commit: base_commit.into(),
        }
    }
}

/// One reproducible agent evaluation input.
#[derive(Debug, Clone)]
pub struct EvalCase {
    pub id: String,
    pub request: ChatCompletionRequest,
    pub fixture: WorkspaceFixture,
}

impl EvalCase {
    pub fn new(
        id: impl Into<String>,
        request: ChatCompletionRequest,
        fixture: WorkspaceFixture,
    ) -> Self {
        Self {
            id: id.into(),
            request,
            fixture,
        }
    }

    pub fn metadata(&self) -> EvalCaseMetadata {
        EvalCaseMetadata {
            id: self.id.clone(),
            model: self.request.model.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalCaseMetadata {
    pub id: String,
    pub model: String,
}

/// Outcome inferred from both the Agent result and its final provider finish reason.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalRunOutcome {
    Completed,
    Incomplete,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalVerdict {
    Passed,
    Failed,
    Inconclusive,
    EvaluatorError,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalFinding {
    pub severity: EvalSeverity,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

impl EvalFinding {
    pub fn new(
        severity: EvalSeverity,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            code: code.into(),
            message: message.into(),
            evidence: None,
        }
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = Some(evidence.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvaluationResult {
    pub evaluator: String,
    pub verdict: EvalVerdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    pub findings: Vec<EvalFinding>,
}

impl EvaluationResult {
    pub fn passed(evaluator: impl Into<String>) -> Self {
        Self {
            evaluator: evaluator.into(),
            verdict: EvalVerdict::Passed,
            score: Some(1.0),
            findings: Vec::new(),
        }
    }

    pub fn failed(evaluator: impl Into<String>, findings: Vec<EvalFinding>) -> Self {
        Self {
            evaluator: evaluator.into(),
            verdict: EvalVerdict::Failed,
            score: Some(0.0),
            findings,
        }
    }

    pub(crate) fn evaluator_error(evaluator: impl Into<String>, message: String) -> Self {
        Self {
            evaluator: evaluator.into(),
            verdict: EvalVerdict::EvaluatorError,
            score: None,
            findings: vec![EvalFinding::new(
                EvalSeverity::Error,
                "evaluator_error",
                message,
            )],
        }
    }
}

/// Complete post-run input available to every evaluator.
pub struct EvaluationContext<'a> {
    pub case: &'a EvalCaseMetadata,
    pub workspace: &'a std::path::Path,
    pub result: Option<&'a AgentRunResult>,
    pub run_error: Option<&'a str>,
    pub outcome: EvalRunOutcome,
    pub duration: Duration,
    pub trace: &'a AgentTrace,
    pub workspace_before: &'a WorkspaceSnapshot,
    pub workspace_after: &'a WorkspaceSnapshot,
    pub workspace_diff: &'a WorkspaceDiff,
}
