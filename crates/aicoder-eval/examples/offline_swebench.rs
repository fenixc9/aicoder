//! Actual isolated generation: one correct patch, one wrong patch and one setup failure.
use aicoder_core::{TurnExecutionConfig, TurnExecutor, model::*, tools::AllowAllApproval};
use aicoder_eval::*;
use anyhow::{Result, ensure};
use async_trait::async_trait;
use clap::Parser;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
#[derive(Parser)]
struct Args {
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long, default_value = "fixture/not-pulled:contract")]
    image: String,
}
struct Model {
    value: usize,
    turn: AtomicUsize,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, name: &str) -> std::result::Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("scripted-swe-contract", name);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: aicoder_core::TurnExecutionContext,
    ) -> std::result::Result<ModelResponse, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let first = self.turn.fetch_add(1, Ordering::SeqCst) == 0;
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: if first {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "value".into(),
                            name: "write_file".into(),
                            arguments: json!({"path":"value.txt","content":format!("{}\n",self.value)}),
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::Assistant, "fixture done")
            },
            stop_reason: if first {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage {
                input_tokens: Some(4),
                output_tokens: Some(1),
                total_tokens: Some(5),
                ..Default::default()
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    ensure!(output.status.success(), "fixture Git failed");
    Ok(String::from_utf8(output.stdout)?)
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let temporary = tempfile::tempdir()?;
    let root = args.output.unwrap_or_else(|| temporary.path().join("run"));
    std::fs::create_dir(&root)?;
    let repositories = root.join("repositories");
    let repository = repositories.join("fixture/repo");
    std::fs::create_dir_all(&repository)?;
    git(&repository, &["init", "--quiet"])?;
    std::fs::write(repository.join("value.txt"), "0\n")?;
    git(&repository, &["add", "value.txt"])?;
    git(
        &repository,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    )?;
    let base = git(&repository, &["rev-parse", "HEAD"])?;
    let rows=(1..=3).map(|i|serde_json::from_value::<SweBenchInstance>(json!({"repo":"fixture/repo","instance_id":format!("fixture__repo-{i}"),"base_commit":base.trim(),"problem_statement":"Set value.txt to exactly 1 followed by a newline.","patch":"PRIVATE_GOLD_NOT_MODEL_INPUT","test_patch":"PRIVATE_TEST_NOT_MODEL_INPUT","hints_text":"Use the existing value file.","version":"fixture","FAIL_TO_PASS":["test_value"],"PASS_TO_PASS":[],"image":args.image,"log_parser":"parse_log_pytest","eval_type":"pass_and_fail","eval_script":"#!/bin/bash\ncd /testbed\necho '>>>>> Start Test Output'\npython /fixture_check.py\necho '>>>>> End Test Output'\n"}))).collect::<std::result::Result<Vec<_>,_>>()?;
    std::fs::write(root.join("dataset.json"), serde_json::to_vec(&rows)?)?;
    let adapter = SweBenchAdapter::new("fixture-model")
        .model_name_or_path("aicoder-official-fixture")
        .repository_source(SweBenchRepositorySource::LocalRoot(repositories));
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let count = factory_calls.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let output = root.join("generation");
    let report = SweBenchBatchRunner::new(
        adapter.clone(),
        SweBenchBatchOptions::new(&output, "offline_swe")
            .dataset("three_case_fixture")
            .execution_pin(benchmark::executable_fingerprint()?)
            .parameters(
                json!({"provider":"scripted_contract","order":"fixed_sequential","workers":1}),
            ),
    )
    .run(
        rows.iter()
            .map(|row| adapter.adapt(row))
            .collect::<Result<_>>()?,
        move |workspace| {
            let index = count.fetch_add(1, Ordering::SeqCst) + 1;
            if index == 3 {
                anyhow::bail!("deliberate fixture setup failure");
            }
            TurnExecutor::builder(Model {
                value: index,
                turn: AtomicUsize::new(0),
                requests: seen.clone(),
            })
            .workspace(workspace)
            .registry(aicoder_runtime::tools::default_registry()?)
            .approval(AllowAllApproval)
            .config(TurnExecutionConfig {
                max_rounds: 4,
                stream: false,
                ..Default::default()
            })
            .build()
        },
    )
    .await?;
    ensure!(
        report.summary.selected == 3
            && report.summary.completed == 2
            && report.summary.failed == 1
            && report.summary.empty_patches == 1,
        "generation fixture classification differs"
    );
    let text = serde_json::to_string(&*requests.lock().unwrap())?;
    ensure!(
        !text.contains("PRIVATE_GOLD")
            && !text.contains("PRIVATE_TEST")
            && !text.contains("eval_script"),
        "hidden grading data reached model"
    );
    SweBenchBatchReport::load_verified(output.join("run.json"))?;
    let resumed = SweBenchBatchRunner::new(
        adapter.clone(),
        SweBenchBatchOptions::new(&output, "offline_swe")
            .dataset("three_case_fixture")
            .execution_pin(benchmark::executable_fingerprint()?)
            .parameters(
                json!({"provider":"scripted_contract","order":"fixed_sequential","workers":1}),
            ),
    )
    .run(
        rows.iter()
            .map(|row| adapter.adapt(row))
            .collect::<Result<_>>()?,
        |_| anyhow::bail!("terminal cases must not execute twice"),
    )
    .await?;
    ensure!(
        resumed.summary.resumed == 3,
        "not all terminal receipts reused"
    );
    println!(
        "{}",
        json!({"dataset":root.join("dataset.json"),"generation":output.join("run.json"),"predictions":output.join("predictions.json"),"summary":resumed.summary,"factory_calls":factory_calls.load(Ordering::SeqCst),"requests":requests.lock().unwrap().len(),"model_network_requests":0,"grading_data_in_prompt":false})
    );
    Ok(())
}
