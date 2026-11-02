use aicoder_core::{TurnExecutionContext, TurnExecutor, model::*, tools::AllowAllApproval};
use aicoder_eval::*;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
struct Model(AtomicUsize);
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, name: &str) -> std::result::Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("fixture", name);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> std::result::Result<ModelResponse, ProviderError> {
        let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: if first {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "fix".into(),
                            name: "write_file".into(),
                            arguments: json!({"path":"fix.txt","content":"fixed\n"}),
                        },
                    }],
                }
            } else {
                Message::text(MessageRole::Assistant, "fixed")
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
fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn fixture(root: &Path) -> (SweBenchAdapter, Vec<SweBenchInstance>) {
    let repo = root.join("fixture/repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet"]);
    std::fs::write(repo.join("base.txt"), "baseline\n").unwrap();
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
    );
    let commit = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let rows=(1..=2).map(|i|serde_json::from_value(json!({"repo":"fixture/repo","instance_id":format!("fixture__repo-{i}"),"base_commit":commit,"problem_statement":format!("Fix task {i}"),"patch":"PRIVATE_GOLD","test_patch":"PRIVATE_TEST","hints_text":"inspect base file","image":"fixture/pinned:1","eval_script":"PRIVATE_GRADER","FAIL_TO_PASS":["test_fix"],"PASS_TO_PASS":[]})).unwrap()).collect();
    (
        SweBenchAdapter::new("fixture-model")
            .model_name_or_path("aicoder-fixture")
            .repository_source(SweBenchRepositorySource::LocalRoot(root.into())),
        rows,
    )
}
fn options(root: &Path) -> SweBenchBatchOptions {
    SweBenchBatchOptions::new(root, "fixture_run")
        .dataset("fixture")
        .execution_pin("a".repeat(64))
}
fn executor(root: &Path) -> Result<TurnExecutor> {
    TurnExecutor::builder(Model(AtomicUsize::new(0)))
        .workspace(root)
        .registry(aicoder_runtime::tools::default_registry()?)
        .approval(AllowAllApproval)
        .build()
}
#[tokio::test]
async fn failed_generation_has_an_empty_prediction_and_stays_in_selected_denominator() {
    let root = tempfile::tempdir().unwrap();
    let (adapter, rows) = fixture(root.path());
    let output = root.path().join("output");
    let report = SweBenchBatchRunner::new(adapter.clone(), options(&output))
        .run(
            rows.iter().map(|r| adapter.adapt(r).unwrap()).collect(),
            |root| {
                static CALLS: AtomicUsize = AtomicUsize::new(0);
                if CALLS.fetch_add(1, Ordering::SeqCst) == 1 {
                    anyhow::bail!("deliberate factory error");
                }
                executor(root)
            },
        )
        .await
        .unwrap();
    assert_eq!(report.summary.selected, 2);
    assert_eq!(report.summary.predictions, 2);
    assert_eq!(report.summary.failed, 1);
    assert_eq!(report.summary.empty_patches, 1);
    assert!(report.summary.total_tokens.is_none());
    let predictions: Vec<SweBenchPrediction> =
        serde_json::from_slice(&std::fs::read(output.join("predictions.json")).unwrap()).unwrap();
    assert_eq!(predictions.len(), 2);
    assert!(predictions[1].model_patch.is_empty());
    SweBenchBatchReport::load_verified(output.join("run.json")).unwrap();
    let resumed = SweBenchBatchRunner::new(adapter.clone(), options(&output))
        .run(
            rows.iter().map(|r| adapter.adapt(r).unwrap()).collect(),
            |_| anyhow::bail!("all terminal receipts, including failure, must be reused"),
        )
        .await
        .unwrap();
    assert_eq!(resumed.summary.resumed, 2);
    assert_eq!(resumed.summary.failed, 1);
    SweBenchBatchReport::load_verified(output.join("run.json")).unwrap();
}
#[tokio::test]
async fn issue_hint_grader_unknown_metadata_and_execution_changes_never_silently_resume() {
    let root = tempfile::tempdir().unwrap();
    let (adapter, rows) = fixture(root.path());
    let output = root.path().join("output");
    SweBenchBatchRunner::new(adapter.clone(), options(&output))
        .run(vec![adapter.adapt(&rows[0]).unwrap()], executor)
        .await
        .unwrap();
    let manifest = std::fs::read(output.join("run.json")).unwrap();
    for variant in ["issue", "hint", "grader", "extra"] {
        let mut changed = rows[0].clone();
        match variant {
            "issue" => changed.problem_statement.push_str(" changed"),
            "hint" => changed.hints_text.push_str(" changed"),
            "grader" => changed.test_patch.push_str(" changed"),
            _ => {
                changed
                    .extra
                    .insert("image".into(), json!("different:image"));
            }
        }
        let result = SweBenchBatchRunner::new(adapter.clone(), options(&output))
            .run(vec![adapter.adapt(&changed).unwrap()], |_| {
                anyhow::bail!("changed input must be rejected before factory")
            })
            .await;
        assert!(result.is_err(), "{variant}");
        assert_eq!(manifest, std::fs::read(output.join("run.json")).unwrap());
    }
    let result = SweBenchBatchRunner::new(
        adapter.clone(),
        options(&output).execution_pin("b".repeat(64)),
    )
    .run(vec![adapter.adapt(&rows[0]).unwrap()], |_| {
        anyhow::bail!("changed code must be rejected")
    })
    .await;
    assert!(result.is_err());
}
#[tokio::test]
async fn missing_or_corrupt_evidence_never_reexecutes_an_admitted_case() {
    for corrupt in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (adapter, rows) = fixture(root.path());
        let output = root.path().join("output");
        SweBenchBatchRunner::new(adapter.clone(), options(&output))
            .run(vec![adapter.adapt(&rows[0]).unwrap()], executor)
            .await
            .unwrap();
        if corrupt {
            std::fs::write(output.join("cases/fixture__repo-1/trace.json"), "[]").unwrap();
        } else {
            std::fs::remove_file(output.join("cases/fixture__repo-1/checkpoint.json")).unwrap();
        }
        let report = SweBenchBatchRunner::new(adapter.clone(), options(&output))
            .run(vec![adapter.adapt(&rows[0]).unwrap()], |_| {
                anyhow::bail!("uncertain evidence must never cause a rerun")
            })
            .await
            .unwrap();
        assert_eq!(report.summary.failed, 1);
        assert_eq!(report.summary.empty_patches, 1);
        assert_eq!(report.summary.resumed, 0);
        SweBenchBatchReport::load_verified(output.join("run.json")).unwrap();
    }
}
#[tokio::test]
async fn generation_reader_cross_checks_prediction_body_against_captured_patch() {
    let root = tempfile::tempdir().unwrap();
    let (adapter, rows) = fixture(root.path());
    let output = root.path().join("output");
    SweBenchBatchRunner::new(adapter.clone(), options(&output))
        .run(vec![adapter.adapt(&rows[0]).unwrap()], executor)
        .await
        .unwrap();
    let mut predictions: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("predictions.json")).unwrap()).unwrap();
    predictions[0]["model_patch"] = json!("forged patch");
    let bytes = serde_json::to_vec(&predictions).unwrap();
    std::fs::write(output.join("predictions.json"), &bytes).unwrap();
    let mut run: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("run.json")).unwrap()).unwrap();
    run["predictions_sha256"] = json!(
        ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    std::fs::write(output.join("run.json"), serde_json::to_vec(&run).unwrap()).unwrap();
    assert!(SweBenchBatchReport::load_verified(output.join("run.json")).is_err());
}
#[test]
fn dataset_rejects_unknown_requested_ids_and_preserves_full_grading_identity() {
    let root = tempfile::tempdir().unwrap();
    let (_, rows) = fixture(root.path());
    let data = SweBenchDataset::from_instances("fixture", rows.clone()).unwrap();
    assert!(
        data.filtered(&SweBenchFilter {
            instance_ids: vec![rows[0].instance_id.clone(), "missing".into()],
            ..Default::default()
        })
        .is_err()
    );
    for bad in [".", "..", "../escape", "bad/id"] {
        let mut row = rows[0].clone();
        row.instance_id = bad.into();
        assert!(SweBenchDataset::from_instances("invalid", vec![row]).is_err());
    }
    let fingerprint = rows[0].fingerprint().unwrap();
    let mut changed = rows[0].clone();
    changed
        .extra
        .insert("future_grading_field".into(), json!("different"));
    assert_ne!(fingerprint, changed.fingerprint().unwrap());
}
#[cfg(unix)]
fn fake_python(root: &Path, wrong_pin: bool, sleep: bool) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("fake-python");
    let calls = root.join("harness-calls");
    let script = format!(
        r#"#!/usr/bin/env python3
import json, pathlib, sys, time
calls=pathlib.Path({calls:?})
a=sys.argv[1:]
if '-c' in a and 'Remove only containers' in a[a.index('-c')+1]:
    with calls.open('a') as f: f.write('cleanup\n')
    print(json.dumps({{'removed':[],'remaining':[],'errors':[]}}))
elif '-c' in a:
    with calls.open('a') as f: f.write('probe\n')
    print(json.dumps({{'version':'5.0.2','package_sha256':{hash:?},'files':65,'python':'3.11.fixture'}}))
else:
    with calls.open('a') as f: f.write('grade\n')
    if {sleep}: time.sleep(30)
    def arg(name): return a[a.index(name)+1]
    rows=json.loads(pathlib.Path(arg('--predictions_path')).read_text())
    ids=a[a.index('--instance_ids')+1:]
    resolved=[r['instance_id'] for r in rows if r['model_patch'] and 'bad' not in r['model_patch']]
    unresolved=[r['instance_id'] for r in rows if r['model_patch'] and 'bad' in r['model_patch']]
    empty=[r['instance_id'] for r in rows if not r['model_patch']]
    report={{'total_instances':len(ids),'submitted_instances':len(rows),'completed_instances':len(resolved)+len(unresolved),'resolved_instances':len(resolved),'unresolved_instances':len(unresolved),'empty_patch_instances':len(empty),'error_instances':0,'completed_ids':resolved+unresolved,'incomplete_ids':[],'empty_patch_ids':empty,'submitted_ids':[r['instance_id'] for r in rows],'resolved_ids':resolved,'unresolved_ids':unresolved,'error_ids':[],'schema_version':2}}
    name=rows[0]['model_name_or_path'].replace('/','__')+'.'+arg('--run_id')+'.json'
    pathlib.Path(name).write_text(json.dumps(report))
    print('fixture grading completed')
"#,
        calls = calls.to_string_lossy(),
        hash = if wrong_pin {
            "0"
        } else {
            SWEBENCH_HARNESS_PACKAGE_SHA256
        },
        sleep = if sleep { "True" } else { "False" }
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}
#[cfg(unix)]
async fn generated_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let (adapter, rows) = fixture(root);
    let output = root.join("generation");
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    SweBenchBatchRunner::new(adapter.clone(), options(&output))
        .run(
            rows.iter().map(|r| adapter.adapt(r).unwrap()).collect(),
            move |root| {
                if count.fetch_add(1, Ordering::SeqCst) == 1 {
                    anyhow::bail!("fixture setup failure");
                }
                executor(root)
            },
        )
        .await
        .unwrap();
    let dataset = root.join("dataset.json");
    std::fs::write(&dataset, serde_json::to_vec(&rows).unwrap()).unwrap();
    (output, dataset)
}
#[cfg(unix)]
#[tokio::test]
async fn linked_grading_keeps_generation_failure_in_denominator_and_reuses_only_complete_receipts()
{
    let root = tempfile::tempdir().unwrap();
    let (generation, dataset) = generated_fixture(root.path()).await;
    let python = fake_python(root.path(), false, false);
    let mut config = SweBenchHarnessConfig::new(
        dataset.to_string_lossy(),
        generation.join("predictions.json"),
        "linked_fixture",
    );
    config.python = python;
    config.report_dir = root.path().join("grading");
    config.generation_manifest = Some(generation.join("run.json"));
    let output = run_swebench_harness_with_context(&config, TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(output.score.selected, 2);
    assert_eq!(output.score.resolved, 1);
    assert_eq!(output.score.resolved_rate, 0.5);
    assert_eq!(output.score.generation_failures, vec!["fixture__repo-2"]);
    assert!(!output.resumed);
    assert!(output.capture.is_some());
    let calls = std::fs::read_to_string(root.path().join("harness-calls")).unwrap();
    assert_eq!(calls, "probe\ngrade\n");
    config.python = "/must-not-run-when-cache-is-verified".into();
    let cached = run_swebench_harness_with_context(&config, TurnExecutionContext::new())
        .await
        .unwrap();
    assert!(cached.resumed);
    assert!(cached.capture.is_none());
    assert_eq!(cached.score.resolved_rate, 0.5);
    assert_eq!(
        std::fs::read_to_string(root.path().join("harness-calls")).unwrap(),
        calls
    );
    SweBenchScore::load_verified(config.report_dir.join("aicoder-swe-grading/linked_fixture"))
        .unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn grading_refuses_changed_rows_predictions_or_incomplete_prior_harness_without_relaunch() {
    let root = tempfile::tempdir().unwrap();
    let (generation, dataset) = generated_fixture(root.path()).await;
    let python = fake_python(root.path(), false, false);
    let mut config = SweBenchHarnessConfig::new(
        dataset.to_string_lossy(),
        generation.join("predictions.json"),
        "guarded_fixture",
    );
    config.python = python;
    config.report_dir = root.path().join("grading");
    config.generation_manifest = Some(generation.join("run.json"));
    run_swebench_harness_with_context(&config, TurnExecutionContext::new())
        .await
        .unwrap();
    let original = std::fs::read(&dataset).unwrap();
    let mut rows: serde_json::Value = serde_json::from_slice(&original).unwrap();
    rows[0]["test_patch"] = json!("different grading answers");
    std::fs::write(&dataset, serde_json::to_vec(&rows).unwrap()).unwrap();
    assert!(
        run_swebench_harness_with_context(&config, TurnExecutionContext::new())
            .await
            .is_err()
    );
    std::fs::write(&dataset, original).unwrap();
    let directory = config
        .report_dir
        .join("aicoder-swe-grading/guarded_fixture");
    std::fs::remove_file(directory.join("grade-receipt.json")).unwrap();
    assert!(
        run_swebench_harness_with_context(&config, TurnExecutionContext::new())
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("harness-calls")).unwrap(),
        "probe\ngrade\n"
    );
}
#[cfg(unix)]
#[tokio::test]
async fn incompatible_harness_is_rejected_before_a_grading_namespace_or_container_launch() {
    let root = tempfile::tempdir().unwrap();
    let (generation, dataset) = generated_fixture(root.path()).await;
    let mut config = SweBenchHarnessConfig::new(
        dataset.to_string_lossy(),
        generation.join("predictions.json"),
        "wrong_pin",
    );
    config.python = fake_python(root.path(), true, false);
    config.report_dir = root.path().join("grading");
    assert!(
        run_swebench_harness_with_context(&config, TurnExecutionContext::new())
            .await
            .is_err()
    );
    assert!(
        !config
            .report_dir
            .join("aicoder-swe-grading/wrong_pin")
            .exists()
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("harness-calls")).unwrap(),
        "probe\n"
    );
}
#[cfg(unix)]
#[tokio::test]
async fn harness_cancellation_awaits_cleanup_and_does_not_turn_into_a_cached_success() {
    let root = tempfile::tempdir().unwrap();
    let (generation, dataset) = generated_fixture(root.path()).await;
    let mut config = SweBenchHarnessConfig::new(
        dataset.to_string_lossy(),
        generation.join("predictions.json"),
        "cancel_fixture",
    );
    config.python = fake_python(root.path(), false, true);
    config.report_dir = root.path().join("grading");
    let scope = TurnExecutionContext::new();
    let worker = scope.clone();
    let owned = config.clone();
    let task = tokio::spawn(async move { run_swebench_harness_with_context(&owned, worker).await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if std::fs::read_to_string(root.path().join("harness-calls"))
                .is_ok_and(|s| s.contains("grade"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    scope.cancel("cancel official owner fixture");
    assert!(task.await.unwrap().is_err());
    assert!(
        run_swebench_harness_with_context(&config, TurnExecutionContext::new())
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("harness-calls")).unwrap(),
        "probe\ngrade\ncleanup\n"
    );
}
#[test]
fn harness_report_rate_uses_total_selection_and_rejects_inconsistent_id_counts() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("official.json");
    let report = json!({"total_instances":3,"submitted_instances":2,"completed_instances":2,"resolved_instances":1,"unresolved_instances":1,"empty_patch_instances":0,"error_instances":0,"completed_ids":["a","b"],"incomplete_ids":["c"],"empty_patch_ids":[],"submitted_ids":["a","b"],"resolved_ids":["a"],"unresolved_ids":["b"],"error_ids":[],"schema_version":2});
    std::fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    let read = SweBenchHarnessReport::load(&path).unwrap();
    assert_eq!(read.resolved_rate(), 1.0 / 3.0);
    assert_eq!(read.submitted_resolved_rate(), 0.5);
    let mut bad = report;
    bad["resolved_instances"] = json!(2);
    std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    assert!(SweBenchHarnessReport::load(path).is_err());
}
#[tokio::test]
async fn missing_host_pin_and_explicit_fresh_mode_never_reuse_or_replace_old_data() {
    let root = tempfile::tempdir().unwrap();
    let (adapter, rows) = fixture(root.path());
    let output = root.path().join("output");
    let initial = SweBenchBatchOptions::new(&output, "no_pin").dataset("fixture");
    SweBenchBatchRunner::new(adapter.clone(), initial.clone())
        .run(vec![adapter.adapt(&rows[0]).unwrap()], executor)
        .await
        .unwrap();
    let before = std::fs::read(output.join("run.json")).unwrap();
    assert!(
        SweBenchBatchRunner::new(adapter.clone(), initial.clone())
            .run(vec![adapter.adapt(&rows[0]).unwrap()], |_| anyhow::bail!(
                "missing pin must not execute"
            ))
            .await
            .is_err()
    );
    assert!(
        SweBenchBatchRunner::new(adapter.clone(), initial.resume(false))
            .run(vec![adapter.adapt(&rows[0]).unwrap()], |_| anyhow::bail!(
                "fresh mode must not overwrite"
            ))
            .await
            .is_err()
    );
    assert_eq!(before, std::fs::read(output.join("run.json")).unwrap());
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn concurrent_generation_lease_prevents_duplicate_agent_admission() {
    struct Waiting(Arc<tokio::sync::Notify>);
    #[async_trait]
    impl ModelProvider for Waiting {
        fn model_spec(&self, name: &str) -> std::result::Result<ModelSpec, ProviderError> {
            let mut spec = ModelSpec::new("waiting", name);
            spec.capabilities.tools = true;
            Ok(spec)
        }
        async fn generate(
            &self,
            _: ModelRequest,
            _: Arc<dyn ModelEventSink>,
            _: aicoder_core::TurnExecutionContext,
        ) -> std::result::Result<ModelResponse, ProviderError> {
            self.0.notify_one();
            std::future::pending().await
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (adapter, rows) = fixture(root.path());
    let output = root.path().join("output");
    let waiting = Arc::new(tokio::sync::Notify::new());
    let ready = waiting.clone();
    let scope = aicoder_core::TurnExecutionContext::new();
    let worker = scope.clone();
    let first = SweBenchBatchRunner::new(adapter.clone(), options(&output));
    let cases = vec![adapter.adapt(&rows[0]).unwrap()];
    let task = tokio::spawn(async move {
        first
            .run_with_context(
                cases,
                move |w| {
                    TurnExecutor::builder(Waiting(ready.clone()))
                        .workspace(w)
                        .build()
                },
                worker,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), waiting.notified())
        .await
        .unwrap();
    assert!(
        SweBenchBatchRunner::new(adapter.clone(), options(&output))
            .run(vec![adapter.adapt(&rows[0]).unwrap()], |_| anyhow::bail!(
                "second owner must not execute"
            ))
            .await
            .is_err()
    );
    scope.cancel("finish lock test");
    task.await.unwrap().unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn special_file_archive_references_fail_without_waiting_for_a_fifo_writer() {
    let root = tempfile::tempdir().unwrap();
    let (adapter, rows) = fixture(root.path());
    let output = root.path().join("output");
    SweBenchBatchRunner::new(adapter.clone(), options(&output))
        .run(vec![adapter.adapt(&rows[0]).unwrap()], executor)
        .await
        .unwrap();
    let trace = output.join("cases/fixture__repo-1/trace.json");
    std::fs::remove_file(&trace).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(trace)
            .status()
            .unwrap()
            .success()
    );
    let (send, receive) = std::sync::mpsc::channel();
    let path = output.join("run.json");
    std::thread::spawn(move || {
        send.send(SweBenchBatchReport::load_verified(path).is_err())
            .unwrap()
    });
    assert!(
        receive
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
    );
}
