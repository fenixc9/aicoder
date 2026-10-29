//! Completion verification policies for agent final-answer candidates.

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;

use crate::model::{Message, ModelUsage};

pub struct CompletionContext<'a> {
    pub workspace: &'a Path,
    pub round: usize,
    pub candidate: &'a Message,
    pub messages: &'a [Message],
    pub usage: &'a ModelUsage,
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
