//! Offline evaluation infrastructure for `aicoder-core` agents.
//!
//! Evaluations run in a temporary workspace, capture a bounded agent event stream with explicit completeness, and apply
//! independent deterministic evaluators after execution. The core agent does not depend on this
//! crate and its behavior is not changed by evaluation.

pub mod benchmark;
mod bounds;
mod command;
mod evaluator;
mod model;
mod provenance;
mod report;
pub use provenance::EvalProvenance;
mod runner;
mod swebench;
mod swebench_batch;
mod swebench_harness;
mod trace;
mod workspace;

pub use evaluator::{CommandEvaluator, Evaluator, TrajectoryEvaluator, WorkspaceDiffEvaluator};
pub use model::{
    EvalCase, EvalCaseMetadata, EvalFinding, EvalRunOutcome, EvalSeverity, EvalVerdict,
    EvaluationContext, EvaluationResult, WorkspaceFixture,
};
pub use report::{
    EvalReport, EvalRunSummary, EvalSuiteReport, EvalSuiteSummary, TrajectorySummary, UsageSummary,
};
pub use runner::EvalRunner;
pub use swebench::{
    SweBenchAdapter, SweBenchCase, SweBenchDataset, SweBenchFilter, SweBenchInstance,
    SweBenchPrediction, SweBenchRepositoryCache, SweBenchRepositorySource,
    write_swebench_predictions,
};
pub use swebench_batch::{
    SweBenchBatchCase, SweBenchBatchCaseStatus, SweBenchBatchOptions, SweBenchBatchReport,
    SweBenchBatchRunner, SweBenchBatchSummary,
};
pub use swebench_harness::{
    SweBenchHarnessConfig, SweBenchHarnessExecution, SweBenchHarnessReport, run_swebench_harness,
};
pub use trace::{AgentTrace, AgentTraceRecorder, TimedAgentEvent, TraceCapture, TraceLimits};
pub use workspace::{WorkspaceDiff, WorkspaceLimits, WorkspaceSnapshot};
