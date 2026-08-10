use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;

use crate::{
    EvalFinding, EvalSeverity, EvalVerdict, EvaluationContext, EvaluationResult, Evaluator,
};

/// Validates required and allowed workspace changes using path prefixes.
#[derive(Debug, Clone)]
pub struct WorkspaceDiffEvaluator {
    name: String,
    allowed_prefixes: Vec<PathBuf>,
    required_changes: Vec<PathBuf>,
    forbid_unlisted: bool,
}

impl WorkspaceDiffEvaluator {
    pub fn new() -> Self {
        Self {
            name: "workspace_diff".to_string(),
            allowed_prefixes: Vec::new(),
            required_changes: Vec::new(),
            forbid_unlisted: false,
        }
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn allow_prefix(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowed_prefixes.push(path.into());
        self
    }

    pub fn require_change(mut self, path: impl Into<PathBuf>) -> Self {
        self.required_changes.push(path.into());
        self
    }

    pub fn forbid_unlisted_changes(mut self, forbid: bool) -> Self {
        self.forbid_unlisted = forbid;
        self
    }
}

impl Default for WorkspaceDiffEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Evaluator for WorkspaceDiffEvaluator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn evaluate(&self, context: &EvaluationContext<'_>) -> Result<EvaluationResult> {
        let changed = context
            .workspace_diff
            .changed_paths()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let mut findings = Vec::new();
        for required in &self.required_changes {
            if !changed.iter().any(|path| path == required) {
                findings.push(EvalFinding::new(
                    EvalSeverity::Error,
                    "required_change_missing",
                    format!("Required path was not changed: {}", required.display()),
                ));
            }
        }
        if self.forbid_unlisted {
            for path in &changed {
                if !self
                    .allowed_prefixes
                    .iter()
                    .any(|allowed| path_allowed(path, allowed))
                {
                    findings.push(EvalFinding::new(
                        EvalSeverity::Error,
                        "unexpected_workspace_change",
                        format!("Unexpected workspace change: {}", path.display()),
                    ));
                }
            }
        }
        let failed = !findings.is_empty();
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

fn path_allowed(path: &Path, allowed: &Path) -> bool {
    path == allowed || path.starts_with(allowed)
}
