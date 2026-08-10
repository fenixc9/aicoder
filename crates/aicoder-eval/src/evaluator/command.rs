use std::{ffi::OsString, time::Duration};

use anyhow::Result;
use async_trait::async_trait;
use tokio::{process::Command, time::timeout};

use crate::{EvalFinding, EvalSeverity, EvaluationContext, EvaluationResult, Evaluator};

const DEFAULT_OUTPUT_LIMIT: usize = 16 * 1024;

/// Runs a trusted command in the post-run workspace and grades its exit status.
pub struct CommandEvaluator {
    name: String,
    program: OsString,
    arguments: Vec<OsString>,
    timeout: Duration,
    output_limit: usize,
}

impl CommandEvaluator {
    pub fn new(name: impl Into<String>, program: impl Into<OsString>) -> Self {
        Self {
            name: name.into(),
            program: program.into(),
            arguments: Vec::new(),
            timeout: Duration::from_secs(120),
            output_limit: DEFAULT_OUTPUT_LIMIT,
        }
    }

    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    pub fn args<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.arguments.extend(arguments.into_iter().map(Into::into));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn output_limit(mut self, bytes: usize) -> Self {
        self.output_limit = bytes;
        self
    }
}

#[async_trait]
impl Evaluator for CommandEvaluator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn evaluate(&self, context: &EvaluationContext<'_>) -> Result<EvaluationResult> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .current_dir(context.workspace)
            .kill_on_drop(true);
        let output = match timeout(self.timeout, command.output()).await {
            Ok(output) => output?,
            Err(_) => {
                return Ok(EvaluationResult::failed(
                    self.name.clone(),
                    vec![EvalFinding::new(
                        EvalSeverity::Error,
                        "command_timeout",
                        format!(
                            "Evaluation command exceeded {:.3} seconds",
                            self.timeout.as_secs_f64()
                        ),
                    )],
                ));
            }
        };
        let evidence = command_evidence(&output.stdout, &output.stderr, self.output_limit);
        if output.status.success() {
            let mut result = EvaluationResult::passed(self.name.clone());
            if !evidence.is_empty() {
                result.findings.push(
                    EvalFinding::new(
                        EvalSeverity::Info,
                        "command_output",
                        "Evaluation command succeeded",
                    )
                    .with_evidence(evidence),
                );
            }
            Ok(result)
        } else {
            Ok(EvaluationResult::failed(
                self.name.clone(),
                vec![
                    EvalFinding::new(
                        EvalSeverity::Error,
                        "command_failed",
                        format!("Evaluation command exited with {}", output.status),
                    )
                    .with_evidence(evidence),
                ],
            ))
        }
    }
}

fn command_evidence(stdout: &[u8], stderr: &[u8], limit: usize) -> String {
    let mut evidence = String::new();
    if !stdout.is_empty() {
        evidence.push_str("stdout:\n");
        evidence.push_str(&String::from_utf8_lossy(stdout));
    }
    if !stderr.is_empty() {
        if !evidence.is_empty() {
            evidence.push('\n');
        }
        evidence.push_str("stderr:\n");
        evidence.push_str(&String::from_utf8_lossy(stderr));
    }
    if evidence.len() <= limit {
        return evidence;
    }
    let mut end = limit;
    while !evidence.is_char_boundary(end) {
        end -= 1;
    }
    evidence.truncate(end);
    evidence.push_str("\n[output truncated]");
    evidence
}
