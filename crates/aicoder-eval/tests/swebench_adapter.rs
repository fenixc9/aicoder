use std::{collections::VecDeque, fs, path::Path, process::Command, sync::Mutex};

use aicoder_core::{
    AgentLoop, ChatCompletionProvider,
    tools::AllowAllApproval,
    types::{ChatCompletionRequest, ChatCompletionResponse},
};
use aicoder_eval::{
    EvalRunner, SweBenchAdapter, SweBenchBatchCaseStatus, SweBenchBatchOptions,
    SweBenchBatchRunner, SweBenchDataset, SweBenchFilter, SweBenchRepositorySource,
    write_swebench_predictions,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;

struct PatchProvider {
    responses: Mutex<VecDeque<ChatCompletionResponse>>,
}

impl PatchProvider {
    fn new() -> Self {
        let tool_arguments = serde_json::to_string(&json!({
            "path": "added.txt",
            "content": "implemented\n"
        }))
        .unwrap();
        let tool_response = json!({
            "id": "tool-response",
            "object": "chat.completion",
            "created": 1,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {"name": "write_file", "arguments": tool_arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let final_response = json!({
            "id": "final-response",
            "object": "chat.completion",
            "created": 2,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "implemented"},
                "finish_reason": "stop"
            }]
        });
        Self {
            responses: Mutex::new(
                vec![
                    serde_json::from_value(tool_response).unwrap(),
                    serde_json::from_value(final_response).unwrap(),
                ]
                .into(),
            ),
        }
    }
}

#[async_trait]
impl ChatCompletionProvider for PatchProvider {
    async fn complete(&self, _request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .context("No SWE-bench test response")
    }
}

#[tokio::test]
async fn adapter_loads_jsonl_hides_gold_and_exports_official_prediction() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("owner").join("repo");
    fs::create_dir_all(&repository).unwrap();
    run_git(&repository, ["init", "--quiet"]);
    run_git(&repository, ["config", "user.email", "eval@example.com"]);
    run_git(&repository, ["config", "user.name", "Evaluation"]);
    fs::write(repository.join("README.md"), "baseline\n").unwrap();
    run_git(&repository, ["add", "README.md"]);
    run_git(&repository, ["commit", "--quiet", "-m", "baseline"]);
    let base_commit = git_output(&repository, ["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let dataset_path = root.path().join("dataset.jsonl");
    let row = json!({
        "repo": "owner/repo",
        "instance_id": "owner__repo-1",
        "base_commit": base_commit,
        "patch": "SECRET_GOLD_PATCH",
        "test_patch": "SECRET_TEST_PATCH",
        "problem_statement": "Create the required implementation.",
        "hints_text": "Add a focused file.",
        "created_at": "2024-01-01",
        "version": "1.0",
        "FAIL_TO_PASS": "[\"tests/test_fix.py::test_case\"]",
        "PASS_TO_PASS": ["tests/test_existing.py::test_case"]
    });
    fs::write(&dataset_path, format!("{row}\n")).unwrap();

    let dataset = SweBenchDataset::load(&dataset_path).unwrap();
    assert_eq!(dataset.len(), 1);
    assert_eq!(dataset.instances[0].fail_to_pass.len(), 1);
    assert_eq!(dataset.instances[0].pass_to_pass.len(), 1);
    let selected = dataset
        .filtered(&SweBenchFilter {
            repositories: vec!["owner/repo".to_string()],
            limit: Some(1),
            ..SweBenchFilter::default()
        })
        .unwrap();
    assert_eq!(selected.len(), 1);
    let adapter = SweBenchAdapter::new("test-model")
        .model_name_or_path("aicoder-test")
        .repository_source(SweBenchRepositorySource::LocalRoot(
            root.path().to_path_buf(),
        ));
    let case = adapter.adapt(&dataset.instances[0]).unwrap();
    let prompt = case.eval_case.request.messages[1]
        .content
        .as_deref()
        .unwrap();
    assert!(prompt.contains("Create the required implementation."));
    assert!(prompt.contains("Add a focused file."));
    assert!(!prompt.contains("SECRET_GOLD_PATCH"));
    assert!(!prompt.contains("SECRET_TEST_PATCH"));

    let runner = EvalRunner::new();
    let report = runner
        .run(&case.eval_case, &|workspace| {
            AgentLoop::builder(PatchProvider::new())
                .workspace(workspace)
                .approval(AllowAllApproval)
                .build()
        })
        .await
        .unwrap();
    let prediction = adapter.prediction(&case, &report).unwrap();

    assert_eq!(prediction.instance_id, "owner__repo-1");
    assert_eq!(prediction.model_name_or_path, "aicoder-test");
    assert!(prediction.model_patch.contains("added.txt"));
    assert!(prediction.model_patch.contains("+implemented"));

    let predictions_path = root.path().join("predictions.json");
    write_swebench_predictions(&predictions_path, std::slice::from_ref(&prediction)).unwrap();
    let encoded: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(predictions_path).unwrap()).unwrap();
    assert_eq!(encoded[0]["instance_id"], "owner__repo-1");
    assert_eq!(encoded[0]["model_name_or_path"], "aicoder-test");
    assert_eq!(encoded[0]["model_patch"], prediction.model_patch);

    let output = root.path().join("batch");
    let options = SweBenchBatchOptions::new(&output, "baseline-test")
        .dataset("fixture")
        .concurrency(2);
    let batch = SweBenchBatchRunner::new(adapter.clone(), options)
        .run(
            vec![adapter.adapt(&dataset.instances[0]).unwrap()],
            |workspace| {
                AgentLoop::builder(PatchProvider::new())
                    .workspace(workspace)
                    .approval(AllowAllApproval)
                    .build()
            },
        )
        .await
        .unwrap();
    assert_eq!(batch.summary.completed, 1);
    assert_eq!(batch.summary.incomplete, 0);
    assert_eq!(batch.summary.failed, 0);
    assert!(output.join("predictions.json").is_file());
    assert!(output.join("cases/owner__repo-1/trace.json").is_file());

    let resumed = SweBenchBatchRunner::new(
        adapter.clone(),
        SweBenchBatchOptions::new(&output, "baseline-test").dataset("fixture"),
    )
    .run(vec![adapter.adapt(&dataset.instances[0]).unwrap()], |_| {
        anyhow::bail!("agent factory must not run when checkpoint is valid")
    })
    .await
    .unwrap();
    assert_eq!(resumed.summary.resumed, 1);
    assert!(resumed.cases[0].resumed);
    assert_eq!(resumed.cases[0].status, SweBenchBatchCaseStatus::Completed);

    let changed = SweBenchBatchRunner::new(
        adapter.clone(),
        SweBenchBatchOptions::new(&output, "baseline-test")
            .dataset("fixture")
            .parameters(json!({"max_rounds": 9})),
    )
    .run(
        vec![adapter.adapt(&dataset.instances[0]).unwrap()],
        |workspace| {
            AgentLoop::builder(PatchProvider::new())
                .workspace(workspace)
                .approval(AllowAllApproval)
                .build()
        },
    )
    .await
    .unwrap();
    assert_eq!(changed.summary.completed, 1);
    assert_eq!(changed.summary.resumed, 0);
    assert!(!changed.cases[0].resumed);
}

fn run_git<const N: usize>(repository: &Path, arguments: [&str; N]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output<const N: usize>(repository: &Path, arguments: [&str; N]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}
