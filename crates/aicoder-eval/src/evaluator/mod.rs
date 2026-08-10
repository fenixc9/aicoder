mod command;
mod trajectory;
mod workspace_diff;

use anyhow::Result;
use async_trait::async_trait;

use crate::{EvaluationContext, EvaluationResult};

pub use command::CommandEvaluator;
pub use trajectory::TrajectoryEvaluator;
pub use workspace_diff::WorkspaceDiffEvaluator;

/// Independent grader applied after an Agent run.
#[async_trait]
pub trait Evaluator: Send + Sync {
    fn name(&self) -> &str;

    async fn evaluate(&self, context: &EvaluationContext<'_>) -> Result<EvaluationResult>;
}
