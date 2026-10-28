//! Completion verification policies for agent final-answer candidates.

use std::{path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use tokio::{process::Command, time::timeout};

use crate::types::{ChatMessage, Usage};

pub struct CompletionContext<'a> {
    pub workspace: &'a Path,
    pub round: usize,
    pub candidate: &'a ChatMessage,
    pub messages: &'a [ChatMessage],
    pub usage: &'a Usage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionVerdict {
    Accepted,
    Rejected { feedback: String },
}

impl CompletionVerdict {
    pub fn rejected(feedback: impl Into<String>) -> Self {
        Self::Rejected {
            feedback: feedback.into(),
        }
    }
}

#[async_trait]
pub trait CompletionVerifier: Send + Sync {
    async fn verify(&self, context: CompletionContext<'_>) -> Result<CompletionVerdict>;
}

/// Compatibility default: every final-answer candidate is accepted.
pub struct AcceptAllCompletionVerifier;

#[async_trait]
impl CompletionVerifier for AcceptAllCompletionVerifier {
    async fn verify(&self, _context: CompletionContext<'_>) -> Result<CompletionVerdict> {
        Ok(CompletionVerdict::Accepted)
    }
}

/// Requires a Git workspace to contain at least one tracked or untracked change.
#[derive(Debug, Clone)]
pub struct WorkspaceChangeVerifier {
    timeout: Duration,
}

impl WorkspaceChangeVerifier {
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(10),
        }
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl Default for WorkspaceChangeVerifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CompletionVerifier for WorkspaceChangeVerifier {
    async fn verify(&self, context: CompletionContext<'_>) -> Result<CompletionVerdict> {
        ensure!(
            context.workspace.join(".git").exists(),
            "Workspace completion verifier requires a Git repository: {}",
            context.workspace.display()
        );
        let output = timeout(
            self.timeout,
            Command::new("git")
                .arg("-C")
                .arg(context.workspace)
                .args(["status", "--porcelain", "--untracked-files=all"])
                .output(),
        )
        .await
        .context("Git workspace verification timed out")?
        .context("Failed to start Git workspace verification")?;
        ensure!(
            output.status.success(),
            "Git workspace verification failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        if output.stdout.is_empty() {
            Ok(CompletionVerdict::rejected(
                "No workspace changes were detected. Continue investigating and implement the requested code change before finishing.",
            ))
        } else {
            Ok(CompletionVerdict::Accepted)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command as StdCommand};

    use tempfile::tempdir;

    use super::*;
    use crate::types::Role;

    fn candidate() -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: Some("done".to_string()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[tokio::test]
    async fn workspace_change_verifier_rejects_clean_and_accepts_modified_repository() {
        let workspace = tempdir().unwrap();
        let status = StdCommand::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(workspace.path())
            .status()
            .unwrap();
        assert!(status.success());
        let message = candidate();
        let usage = Usage::default();
        let verifier = WorkspaceChangeVerifier::new();

        let clean = verifier
            .verify(CompletionContext {
                workspace: workspace.path(),
                round: 1,
                candidate: &message,
                messages: std::slice::from_ref(&message),
                usage: &usage,
            })
            .await
            .unwrap();
        assert!(matches!(clean, CompletionVerdict::Rejected { .. }));

        fs::write(workspace.path().join("change.txt"), "changed").unwrap();
        let changed = verifier
            .verify(CompletionContext {
                workspace: workspace.path(),
                round: 2,
                candidate: &message,
                messages: std::slice::from_ref(&message),
                usage: &usage,
            })
            .await
            .unwrap();
        assert_eq!(changed, CompletionVerdict::Accepted);
    }
}
