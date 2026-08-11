use std::{fs, path::Path};

use aicoder_core::{
    ChatCompletionProvider, TurnExecutionConfig, TurnExecutor,
    tools::ToolRegistry,
    types::{ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Role},
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
impl ChatCompletionProvider for StaticProvider {
    async fn complete(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        Ok(serde_json::from_value(serde_json::json!({
            "id": "eval-response",
            "object": "chat.completion",
            "created": 1,
            "model": request.model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "done"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 4,
                "completion_tokens": 1,
                "total_tokens": 5
            }
        }))?)
    }
}

#[async_trait]
impl ChatCompletionProvider for ToolOnlyProvider {
    async fn complete(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        Ok(serde_json::from_value(serde_json::json!({
            "id": "tool-response",
            "object": "chat.completion",
            "created": 1,
            "model": request.model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\":\"missing\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 6,
                "completion_tokens": 1,
                "total_tokens": 7
            }
        }))?)
    }
}

fn request() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "eval-model".to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: Some("inspect the fixture".to_string()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        temperature: None,
        top_p: None,
        max_tokens: None,
        seed: None,
        tools: None,
        tool_choice: None,
        stream: None,
        stream_options: None,
        stop: None,
        response_format: None,
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
    assert_eq!(report.run.usage.total_tokens, 5);
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
async fn failed_agent_run_retains_provider_usage_from_trace() {
    let case = EvalCase::new("round-limit", request(), WorkspaceFixture::Empty);
    let report = EvalRunner::new()
        .run(&case, &|workspace| {
            TurnExecutor::builder(ToolOnlyProvider)
                .workspace(workspace)
                .config(TurnExecutionConfig {
                    max_rounds: 1,
                    stream: false,
                })
                .build()
        })
        .await
        .unwrap();

    assert_eq!(report.run.outcome, EvalRunOutcome::Failed);
    assert_eq!(report.run.usage.total_tokens, 7);
    assert!(report.run.usage.provider_reported);
    assert!(
        report
            .run
            .error
            .unwrap()
            .contains("maximum of 1 model rounds")
    );
}
