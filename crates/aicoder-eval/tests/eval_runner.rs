use aicoder_core::model::{ModelOptions, ModelToolChoice};
use std::sync::Arc;
use std::{fs, path::Path};

use aicoder_core::{
    ModelProvider, TurnExecutionConfig, TurnExecutor,
    model::{Message, MessageRole, ModelRequest, ModelResponse},
    tools::ToolRegistry,
};
use aicoder_eval::{
    CommandEvaluator, EvalCase, EvalRunOutcome, EvalRunner, EvalVerdict, EvaluationContext,
    EvaluationResult, Evaluator, TrajectoryEvaluator, WorkspaceDiffEvaluator, WorkspaceFixture,
};
use anyhow::Result;
use async_trait::async_trait;

struct StaticProvider;

struct ToolOnlyProvider;

struct BrokenEvaluator;

#[async_trait]
impl Evaluator for BrokenEvaluator {
    fn name(&self) -> &str {
        "broken"
    }

    async fn evaluate(&self, _context: &EvaluationContext<'_>) -> Result<EvaluationResult> {
        anyhow::bail!("grader unavailable")
    }
}

#[async_trait]
impl ModelProvider for StaticProvider {
    fn model_spec(
        &self,
        model: &str,
    ) -> std::result::Result<aicoder_core::model::ModelSpec, aicoder_core::model::ProviderError>
    {
        let mut spec = aicoder_core::model::ModelSpec::new("scripted", model);
        spec.capabilities = aicoder_core::model::ModelCapabilities {
            reasoning_efforts: Vec::new(),
            tools: true,
            images: true,
            reasoning_history: true,
            temperature: true,
            top_p: true,
            seed: true,
            stop_sequences: true,
            structured_output: true,
        };
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        events: Arc<dyn aicoder_core::model::ModelEventSink>,
        _context: aicoder_core::TurnExecutionContext,
    ) -> std::result::Result<ModelResponse, aicoder_core::model::ProviderError> {
        let result: anyhow::Result<ModelResponse> = async {
        Ok(serde_json::from_value(serde_json::json!({"id":"eval-response","model":request.model,"message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"stop_reason":{"reason":"complete"},"usage":{"input_tokens":4,"output_tokens":1,"total_tokens":5,"cached_input_tokens":null,"cache_write_tokens":null}}))?)
    }.await;
        match result {
            Ok(response) => {
                aicoder_core::model::emit_response(&response, events.as_ref());
                Ok(response)
            }
            Err(error) => Err(aicoder_core::model::ProviderError::new(
                "scripted",
                aicoder_core::model::ProviderErrorKind::Other,
                error.to_string(),
            )),
        }
    }
}

#[async_trait]
impl ModelProvider for ToolOnlyProvider {
    fn model_spec(
        &self,
        model: &str,
    ) -> std::result::Result<aicoder_core::model::ModelSpec, aicoder_core::model::ProviderError>
    {
        let mut spec = aicoder_core::model::ModelSpec::new("scripted", model);
        spec.capabilities = aicoder_core::model::ModelCapabilities {
            reasoning_efforts: Vec::new(),
            tools: true,
            images: true,
            reasoning_history: true,
            temperature: true,
            top_p: true,
            seed: true,
            stop_sequences: true,
            structured_output: true,
        };
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        events: Arc<dyn aicoder_core::model::ModelEventSink>,
        _context: aicoder_core::TurnExecutionContext,
    ) -> std::result::Result<ModelResponse, aicoder_core::model::ProviderError> {
        let result: anyhow::Result<ModelResponse> = async {
        Ok(serde_json::from_value(serde_json::json!({"id":"tool-response","model":request.model,"message":{"role":"assistant","content":[{"type":"tool_call","call":{"id":"call-1","name":"read_file","arguments":{"path": "missing"}}}]},"stop_reason":{"reason":"tool_calls"},"usage":{"input_tokens":6,"output_tokens":1,"total_tokens":7,"cached_input_tokens":null,"cache_write_tokens":null}}))?)
    }.await;
        match result {
            Ok(response) => {
                aicoder_core::model::emit_response(&response, events.as_ref());
                Ok(response)
            }
            Err(error) => Err(aicoder_core::model::ProviderError::new(
                "scripted",
                aicoder_core::model::ProviderErrorKind::Other,
                error.to_string(),
            )),
        }
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        model: "eval-model".to_string(),
        messages: vec![Message::text(
            MessageRole::User,
            "inspect the fixture".to_string(),
        )],
        options: ModelOptions::default(),
        tools: Vec::new(),
        tool_choice: ModelToolChoice::Auto,
        stream: false,
    }
}

fn build_executor(workspace: &Path) -> Result<TurnExecutor> {
    TurnExecutor::builder(StaticProvider)
        .workspace(workspace)
        .registry(ToolRegistry::default())
        .build()
}

#[tokio::test]
async fn runner_captures_trace_and_applies_evaluators() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("seed.txt"), "fixture").unwrap();
    let case = EvalCase::new(
        "basic-eval",
        request(),
        WorkspaceFixture::CopyFrom(fixture.path().to_path_buf()),
    );
    let runner = EvalRunner::new()
        .evaluator(CommandEvaluator::new("fixture_exists", "test").args(["-f", "seed.txt"]))
        .evaluator(TrajectoryEvaluator::new().max_rounds(1).max_tool_calls(0))
        .evaluator(WorkspaceDiffEvaluator::new());

    let report = runner.run(&case, &build_executor).await.unwrap();

    assert_eq!(report.verdict, EvalVerdict::Passed);
    assert_eq!(report.run.outcome, EvalRunOutcome::Completed);
    assert_eq!(report.run.usage.total_tokens, Some(5));
    assert!(report.run.usage.provider_reported);
    assert_eq!(report.trajectory.rounds, 1);
    assert_eq!(report.trajectory.state_transitions, 4);
    assert_eq!(report.trajectory.final_state.as_deref(), Some("completed"));
    assert_eq!(report.trajectory.model_requests, 1);
    assert!(report.workspace_diff.is_empty());
    assert_eq!(report.evaluations.len(), 3);
    let json = report.to_json_pretty().unwrap();
    assert!(json.contains("\"basic-eval\""));
    assert!(json.contains("\"completed\""));
}

#[tokio::test]
async fn suite_aggregates_repeated_runs_and_required_change_failures() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("seed.txt"), "fixture").unwrap();
    let case = EvalCase::new(
        "repeat-eval",
        request(),
        WorkspaceFixture::CopyFrom(fixture.path().to_path_buf()),
    );
    let runner =
        EvalRunner::new().evaluator(WorkspaceDiffEvaluator::new().require_change("seed.txt"));

    let report = runner.run_many(&case, 2, &build_executor).await.unwrap();

    assert_eq!(report.summary.runs, 2);
    assert_eq!(report.summary.failed, 2);
    assert_eq!(report.summary.pass_rate, 0.0);
    assert!(
        report
            .runs
            .iter()
            .all(|run| run.verdict == EvalVerdict::Failed)
    );
}

#[tokio::test]
async fn evaluator_errors_are_reported_without_losing_the_run() {
    let case = EvalCase::new("broken-grader", request(), WorkspaceFixture::Empty);
    let runner = EvalRunner::new().evaluator(BrokenEvaluator);

    let report = runner.run(&case, &build_executor).await.unwrap();

    assert_eq!(report.run.outcome, EvalRunOutcome::Completed);
    assert_eq!(report.verdict, EvalVerdict::EvaluatorError);
    assert_eq!(report.evaluations[0].verdict, EvalVerdict::EvaluatorError);
    assert!(
        report.evaluations[0].findings[0]
            .message
            .contains("grader unavailable")
    );
}

#[tokio::test]
async fn limited_agent_run_retains_progress_and_usage_from_outcome() {
    let case = EvalCase::new("round-limit", request(), WorkspaceFixture::Empty);
    let report = EvalRunner::new()
        .run(&case, &|workspace| {
            TurnExecutor::builder(ToolOnlyProvider)
                .workspace(workspace)
                .config(TurnExecutionConfig {
                    max_rounds: 1,
                    stream: false,
                    ..Default::default()
                })
                .build()
        })
        .await
        .unwrap();

    assert_eq!(report.run.outcome, EvalRunOutcome::Incomplete);
    assert_eq!(report.run.usage.total_tokens, Some(7));
    assert!(matches!(
        report.run.stop,
        Some(aicoder_core::RunStop::Budget(_))
    ));
    assert_eq!(report.progress.messages.len(), 3);
    assert!(report.run.usage.provider_reported);
    assert!(
        report
            .run
            .error
            .unwrap()
            .contains("maximum of 1 model rounds")
    );
}
