use aicoder_core::{
    RunBudget, TurnExecutionConfig, TurnExecutionContext, TurnExecutor, model::*,
    tools::AllowAllApproval,
};
use aicoder_eval::{benchmark::*, *};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
fn source() -> SourceStamp {
    SourceStamp {
        revision: "1".repeat(40),
        dirty: false,
        source_sha256: "2".repeat(64),
        file_count: 1,
        scope: "test-declared source".into(),
        executable_sha256: None,
        grader_rustc: Some("test compiler pin".into()),
        platform: "linux/test".into(),
    }
}
fn options(root: &Path, ids: &[&str]) -> BenchOptions {
    let mut options = BenchOptions::new(root, source(), BenchMode::ScriptedContract);
    options.selected_ids = ids.iter().map(|s| s.to_string()).collect();
    options
}
fn executor(
    case: &CodingCase,
    root: &Path,
    variant: ScriptVariant,
    rounds: usize,
) -> anyhow::Result<TurnExecutor> {
    TurnExecutor::builder(ScriptedCodingProvider::new(case, variant))
        .workspace(root)
        .registry(aicoder_runtime::tools::default_registry()?)
        .approval(AllowAllApproval)
        .config(TurnExecutionConfig {
            max_rounds: rounds,
            stream: false,
            budget: RunBudget {
                max_tool_calls: Some(8),
                max_duration: Some(Duration::from_secs(5)),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
}
async fn run(root: &Path, ids: &[&str], variant: ScriptVariant) -> BenchReport {
    BenchRunner::new(CodingSuite::builtin().unwrap(), options(root, ids))
        .unwrap()
        .run(
            |c| c.request("offline", ModelOptions::default(), None),
            |c, w| executor(c, w, variant, 4),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn real_tools_hidden_rust_and_json_graders_produce_verifiable_observation_only_archives() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    let report = run(&root, &[], ScriptVariant::Correct).await;
    assert_eq!(report.summary.selected, 4);
    assert_eq!(report.summary.passed, 4);
    assert_eq!(report.summary.total_tool_calls, Some(8));
    assert_eq!(report.summary.total_tokens, Some(60));
    assert!(report.summary.cost.is_none());
    let verified = BenchReport::verify(&root).unwrap();
    assert_eq!(verified.summary, report.summary);
    let before = std::fs::read(root.join("manifest.json")).unwrap();
    let trace = BenchReport::replay_case(&root, "inclusive_range", 0).unwrap();
    trace.validate().unwrap();
    assert_eq!(trace.summary().tool_calls, 2);
    assert_eq!(before, std::fs::read(root.join("manifest.json")).unwrap());
    for case in report.cases {
        assert!(case.trace_valid && case.input_sha256.is_some());
        assert!(!case.memory.includes_children);
        assert!(case.memory.samples > 0);
        assert!(case.memory.sampled_peak_rss_kib.is_some());
    }
}
#[tokio::test]
async fn semantic_hidden_rust_tests_accept_an_alternative_correct_implementation() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    let mut suite = CodingSuite::builtin().unwrap();
    suite.cases[0].script[1].arguments["edits"][0]["new_text"] = json!(
        "pub fn contains(value:i32, lower:i32, upper:i32)->bool { (lower..=upper).contains(&value) }\n"
    );
    let report = BenchRunner::new(suite, options(&root, &["inclusive_range"]))
        .unwrap()
        .run(
            |c| c.request("offline", ModelOptions::default(), None),
            |c, w| executor(c, w, ScriptVariant::Correct, 4),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.summary.passed, 1);
    BenchReport::verify(root).unwrap();
}
#[tokio::test]
async fn unchanged_solutions_and_provider_failures_remain_in_the_full_denominator() {
    let parent = tempfile::tempdir().unwrap();
    for variant in [
        ScriptVariant::Noop,
        ScriptVariant::ProviderError,
        ScriptVariant::Limit,
    ] {
        let root = parent.path().join(format!("{variant:?}"));
        let report = run(&root, &[], variant).await;
        assert_eq!(report.summary.selected, 4);
        assert_eq!(report.summary.passed, 0);
        assert_eq!(report.summary.pass_rate, 0.0);
        if variant == ScriptVariant::ProviderError {
            assert!(report.summary.total_tokens.is_none());
            assert!(report.cases.iter().all(|c| c.stop.is_some()));
        }
        if variant == ScriptVariant::Limit {
            assert_eq!(report.summary.incomplete, 4);
        }
        BenchReport::verify(root).unwrap();
    }
}
#[tokio::test]
async fn factory_failure_does_not_stop_later_cases_or_invent_zero_consumption() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    let report = BenchRunner::new(CodingSuite::builtin().unwrap(), options(&root, &[]))
        .unwrap()
        .run(
            |c| c.request("offline", ModelOptions::default(), None),
            |c, w| {
                if c.id == "checked_clamp" {
                    anyhow::bail!("deliberate factory error");
                }
                executor(c, w, ScriptVariant::Correct, 4)
            },
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.summary.selected, 4);
    assert_eq!(report.summary.passed, 3);
    assert_eq!(report.summary.infrastructure_errors, 1);
    assert_eq!(report.summary.pass_rate, 0.75);
    assert!(report.summary.total_tokens.is_none());
    assert!(report.summary.total_tool_calls.is_none());
    BenchReport::verify(root).unwrap();
}
#[tokio::test]
async fn cancellation_records_unstarted_cases_and_never_admits_the_factory() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    let context = TurnExecutionContext::new();
    context.cancel("cancel before first case");
    let report = BenchRunner::new(CodingSuite::builtin().unwrap(), options(&root, &[]))
        .unwrap()
        .run(
            |c| c.request("offline", ModelOptions::default(), None),
            |_, _| panic!("cancelled case must not execute"),
            context,
        )
        .await
        .unwrap();
    assert_eq!(report.summary.selected, 4);
    assert_eq!(report.summary.not_run, 4);
    assert_eq!(report.summary.passed, 0);
    assert!(report.cancelled);
    BenchReport::verify(root).unwrap();
}
#[tokio::test]
async fn an_existing_archive_cannot_be_overwritten_or_execute_again() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    run(&root, &["retry_config"], ScriptVariant::Correct).await;
    let before = std::fs::read(root.join("manifest.json")).unwrap();
    let repeated = BenchRunner::new(
        CodingSuite::builtin().unwrap(),
        options(&root, &["retry_config"]),
    )
    .unwrap()
    .run(
        |c| c.request("offline", ModelOptions::default(), None),
        |_, _| panic!("existing output must fail before execution"),
        TurnExecutionContext::new(),
    )
    .await;
    assert!(repeated.is_err());
    assert_eq!(before, std::fs::read(root.join("manifest.json")).unwrap());
}
#[tokio::test]
async fn comparison_rejects_different_actual_budgets_and_accepts_identical_inputs() {
    let parent = tempfile::tempdir().unwrap();
    let left = run(
        &parent.path().join("left"),
        &["retry_config"],
        ScriptVariant::Correct,
    )
    .await;
    let right = run(
        &parent.path().join("right"),
        &["retry_config"],
        ScriptVariant::Correct,
    )
    .await;
    let comparison = BenchComparison::compare(&left, &right).unwrap();
    assert_eq!(comparison.both_passed, 1);
    let different = BenchRunner::new(
        CodingSuite::builtin().unwrap(),
        options(&parent.path().join("different"), &["retry_config"]),
    )
    .unwrap()
    .run(
        |c| c.request("offline", ModelOptions::default(), None),
        |c, w| executor(c, w, ScriptVariant::Correct, 5),
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    assert!(BenchComparison::compare(&left, &different).is_err());
    let mut omitted = right.clone();
    omitted.cases.clear();
    assert!(BenchComparison::compare(&left, &omitted).is_err());
}
fn manifest(root: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap()
}
fn save_manifest(root: &Path, value: &Value) {
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}
#[tokio::test]
async fn corrupt_artifacts_missing_cases_and_forged_summary_are_rejected() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    run(&root, &["retry_config"], ScriptVariant::Correct).await;
    let original = manifest(&root);
    let mut changed = original.clone();
    changed["summary"]["passed"] = json!(100);
    save_manifest(&root, &changed);
    assert!(BenchReport::verify(&root).is_err());
    changed = original.clone();
    changed["cases"] = json!([]);
    save_manifest(&root, &changed);
    assert!(BenchReport::verify(&root).is_err());
    changed = original.clone();
    changed["cases"][0]["artifacts"][0]["path"] = json!("../secret.json");
    save_manifest(&root, &changed);
    assert!(BenchReport::verify(&root).is_err());
    save_manifest(&root, &original);
    let trace = root.join("cases/retry_config-0/trace.json");
    let mut bytes = std::fs::read(&trace).unwrap();
    bytes.push(b' ');
    std::fs::write(trace, bytes).unwrap();
    assert!(BenchReport::verify(&root).is_err());
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn verifier_refuses_identical_bytes_through_leaf_and_parent_symlinks() {
    use std::os::unix::fs::symlink;
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    run(&root, &["retry_config"], ScriptVariant::Correct).await;
    let leaf = root.join("cases/retry_config-0/trace.json");
    let external = parent.path().join("external.json");
    std::fs::rename(&leaf, &external).unwrap();
    symlink(&external, &leaf).unwrap();
    assert!(BenchReport::verify(&root).is_err());
    std::fs::remove_file(&leaf).unwrap();
    std::fs::rename(external, &leaf).unwrap();
    let case = root.join("cases/retry_config-0");
    let moved = parent.path().join("moved-case");
    std::fs::rename(&case, &moved).unwrap();
    symlink(&moved, &case).unwrap();
    assert!(BenchReport::verify(&root).is_err());
}
struct Inspect {
    inner: ScriptedCodingProvider,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}
#[async_trait]
impl ModelProvider for Inspect {
    fn model_spec(&self, name: &str) -> Result<ModelSpec, ProviderError> {
        self.inner.model_spec(name)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        ctx: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.generate(request, sink, ctx).await
    }
}
#[tokio::test]
async fn model_never_receives_the_hidden_test_source_or_grading_manifest() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let report = BenchRunner::new(
        CodingSuite::builtin().unwrap(),
        options(&root, &["inclusive_range"]),
    )
    .unwrap()
    .run(
        |c| c.request("offline", ModelOptions::default(), None),
        |c, w| {
            TurnExecutor::builder(Inspect {
                inner: ScriptedCodingProvider::new(c, ScriptVariant::Correct),
                requests: requests.clone(),
            })
            .workspace(w)
            .registry(aicoder_runtime::tools::default_registry()?)
            .approval(AllowAllApproval)
            .build()
        },
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.summary.passed, 1);
    let text = serde_json::to_string(&*requests.lock().unwrap()).unwrap();
    assert!(!text.contains("fn bounds()"));
    assert!(!text.contains("candidate/src/lib.rs"));
    assert!(!text.contains("grading\""));
    let count = requests.lock().unwrap().len();
    BenchReport::replay_case(root, "inclusive_range", 0).unwrap();
    assert_eq!(requests.lock().unwrap().len(), count);
}
#[test]
fn suites_reject_traversal_duplicate_ids_and_oversized_inputs_before_execution() {
    for path in [
        "../secret",
        "/tmp/file",
        "a/../b",
        "a//b",
        "a/.git/config",
        "bad\nname",
    ] {
        let mut suite = CodingSuite::builtin().unwrap();
        suite.cases[0].files.insert(path.into(), "bad".into());
        assert!(suite.validate().is_err(), "{path}");
    }
    let mut suite = CodingSuite::builtin().unwrap();
    suite.cases.push(suite.cases[0].clone());
    assert!(suite.validate().is_err());
    let mut suite = CodingSuite::builtin().unwrap();
    suite.cases[0].prompt = "x".repeat(16385);
    assert!(suite.validate().is_err());
}
#[test]
fn snapshots_enforce_file_total_entry_depth_and_link_semantics() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a"), "12345").unwrap();
    std::fs::write(root.path().join("b"), "67890").unwrap();
    let mut limits = WorkspaceLimits {
        max_file_bytes: 4,
        ..Default::default()
    };
    assert!(WorkspaceSnapshot::capture_with_limits(root.path(), limits).is_err());
    limits = WorkspaceLimits::default();
    limits.max_total_bytes = 8;
    assert!(WorkspaceSnapshot::capture_with_limits(root.path(), limits).is_err());
    limits = WorkspaceLimits::default();
    limits.max_entries = 1;
    assert!(WorkspaceSnapshot::capture_with_limits(root.path(), limits).is_err());
    let nested = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(nested.path().join("a/b/c")).unwrap();
    limits = WorkspaceLimits::default();
    limits.max_depth = 1;
    assert!(WorkspaceSnapshot::capture_with_limits(nested.path(), limits).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let before = WorkspaceSnapshot::capture(root.path()).unwrap();
        std::fs::set_permissions(
            root.path().join("a"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let after = WorkspaceSnapshot::capture(root.path()).unwrap();
        assert_ne!(before.fingerprint(), after.fingerprint());
        assert_eq!(WorkspaceDiff::between(&before, &after).modified, vec!["a"]);
        symlink("/outside/never/read", root.path().join("link")).unwrap();
        assert!(
            WorkspaceSnapshot::capture(root.path())
                .unwrap()
                .file_content("link")
                .is_none()
        );
    }
}
#[tokio::test]
async fn trace_overflow_is_an_evaluator_error_and_bounded_replay_cannot_fake_completeness() {
    let parent = tempfile::tempdir().unwrap();
    let fixture = parent.path().join("fixture");
    std::fs::create_dir(&fixture).unwrap();
    std::fs::write(fixture.join("config.json"), "{}").unwrap();
    let suite = CodingSuite::builtin().unwrap();
    let case = &suite.cases[3];
    let eval = EvalCase::new(
        "overflow",
        case.request("offline", ModelOptions::default(), None),
        WorkspaceFixture::CopyFrom(fixture),
    );
    let report = EvalRunner::new()
        .trace_limits(TraceLimits {
            max_events: 1,
            max_bytes: 4096,
        })
        .unwrap()
        .run(&eval, &|w| executor(case, w, ScriptVariant::Noop, 4))
        .await
        .unwrap();
    assert_eq!(report.verdict, EvalVerdict::EvaluatorError);
    assert_eq!(report.trace.events.len(), 1);
    assert!(!report.trace_capture.complete());
    assert!(report.trace_capture.dropped_events > 0);
    let path = parent.path().join("trace.json");
    report.trace.write_json(&path).unwrap();
    assert!(AgentTrace::read_json(path, TraceLimits::default()).is_err());
}
#[tokio::test]
async fn a_completed_but_ungraded_run_is_inconclusive() {
    let suite = CodingSuite::builtin().unwrap();
    let case = &suite.cases[3];
    let eval = EvalCase::new(
        "ungraded",
        case.request("offline", ModelOptions::default(), None),
        WorkspaceFixture::Empty,
    );
    let report = EvalRunner::new()
        .run(&eval, &|w| executor(case, w, ScriptVariant::Noop, 4))
        .await
        .unwrap();
    assert_eq!(report.verdict, EvalVerdict::Inconclusive);
}
#[tokio::test]
async fn post_run_snapshot_exhaustion_retains_the_completed_progress_and_usage() {
    struct LargeFile;
    #[async_trait]
    impl aicoder_core::tools::ExecutableTool for LargeFile {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "read_file".into(),
                description: None,
                parameters: json!({"type":"object"}),
            }
        }
        fn capability(&self) -> aicoder_core::tools::ToolCapability {
            aicoder_core::tools::ToolCapability::ReadOnly
        }
        async fn execute(
            &self,
            context: &aicoder_core::tools::ToolContext,
            _: Value,
        ) -> Result<aicoder_core::tools::ToolSuccess, aicoder_core::tools::ToolFailure> {
            std::fs::File::create(context.workspace_root().join("oversized.bin"))
                .unwrap()
                .set_len(17 * 1024 * 1024)
                .unwrap();
            Ok(aicoder_core::tools::ToolSuccess::new(
                json!({"created":true}),
            ))
        }
    }
    let mut case = CodingSuite::builtin().unwrap().cases[3].clone();
    case.script.truncate(1);
    let eval = EvalCase::new(
        "snapshot_limit",
        case.request("offline", ModelOptions::default(), None),
        WorkspaceFixture::Empty,
    );
    let report = EvalRunner::new()
        .run(&eval, &|root| {
            let mut registry = aicoder_core::tools::ToolRegistry::default();
            registry.register(LargeFile)?;
            TurnExecutor::builder(ScriptedCodingProvider::new(&case, ScriptVariant::Correct))
                .workspace(root)
                .registry(registry)
                .build()
        })
        .await
        .unwrap();
    assert_eq!(report.run.outcome, EvalRunOutcome::Completed);
    assert_eq!(report.progress.tool_calls, 1);
    assert_eq!(report.run.usage.total_tokens, Some(10));
    assert_eq!(report.verdict, EvalVerdict::EvaluatorError);
    assert!(!report.workspace_capture_complete);
    assert!(
        report
            .evaluations
            .iter()
            .any(|e| e.evaluator == "workspace_observation")
    );
    report.trace.validate().unwrap();
}
#[tokio::test]
async fn grader_arguments_are_literal_and_timeout_kills_the_owned_process_group() {
    let parent = tempfile::tempdir().unwrap();
    let marker = parent.path().join("must-not-exist");
    let child_pid = parent.path().join("child.pid");
    let case = CodingSuite::builtin().unwrap().cases[3].clone();
    let eval = EvalCase::new(
        "grader",
        case.request("offline", ModelOptions::default(), None),
        WorkspaceFixture::Empty,
    );
    let literal = format!("$(touch {}) `touch {}`", marker.display(), marker.display());
    let report = EvalRunner::new()
        .evaluator(CommandEvaluator::new("quoted", "printf").args(["%s", &literal]))
        .run(&eval, &|w| executor(&case, w, ScriptVariant::Noop, 4))
        .await
        .unwrap();
    assert_eq!(report.verdict, EvalVerdict::Passed);
    assert!(!marker.exists());
    assert!(
        report.evaluations[0].findings[0]
            .evidence
            .as_ref()
            .unwrap()
            .contains(&literal)
    );
    let script = format!("sleep 30 & printf %s $! > '{}'; wait", child_pid.display());
    let report = EvalRunner::new()
        .evaluator(
            CommandEvaluator::new("timeout", "/bin/bash")
                .args(["-c", &script])
                .timeout(Duration::from_millis(150)),
        )
        .run(&eval, &|w| executor(&case, w, ScriptVariant::Noop, 4))
        .await
        .unwrap();
    assert_eq!(report.verdict, EvalVerdict::Failed);
    assert_eq!(report.evaluations[0].findings[0].code, "command_timeout");
    let pid = std::fs::read_to_string(child_pid).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            if stat.as_ref().map_or(true, |text| {
                text.rsplit_once(')')
                    .is_some_and(|(_, tail)| tail.trim_start().starts_with('Z'))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[test]
fn offline_cli_cannot_read_provider_configuration_without_explicit_live_mode() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_aicoder-bench"))
        .args([
            "run",
            "--output",
            "/must-not-be-created",
            "--env-file",
            "/unread-provider-file",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("requires explicit --live"));
}
#[test]
fn source_stamps_include_dirty_untracked_embedded_inputs_and_skip_environment_files() {
    let root = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(root.path())
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    };
    git(&["init", "--quiet"]);
    git(&[
        "-c",
        "user.name=fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "fixture",
    ]);
    std::fs::create_dir_all(root.path().join("crates/fixture")).unwrap();
    std::fs::write(root.path().join("crates/fixture/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.path().join(".env"), "MUST_NOT_READ=private\n").unwrap();
    let first = SourceStamp::capture(root.path()).unwrap();
    assert!(first.dirty);
    assert_eq!(first.file_count, 1);
    std::fs::write(
        root.path().join("crates/fixture/cases.json"),
        "{\"case\":1}",
    )
    .unwrap();
    let second = SourceStamp::capture(root.path()).unwrap();
    assert_eq!(second.file_count, 2);
    assert_ne!(first.source_sha256, second.source_sha256);
}
#[tokio::test]
async fn partial_artifact_failure_never_repeats_operations_or_claims_success() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    let admitted = AtomicUsize::new(0);
    let report = BenchRunner::new(
        CodingSuite::builtin().unwrap(),
        options(&root, &["retry_config"]),
    )
    .unwrap()
    .run(
        |c| c.request("offline", ModelOptions::default(), None),
        |c, w| {
            admitted.fetch_add(1, Ordering::SeqCst);
            let case = root.join("cases/retry_config-0");
            std::fs::create_dir_all(&case)?;
            std::fs::write(case.join("trace.json"), "reserved by injected conflict")?;
            executor(c, w, ScriptVariant::Correct, 4)
        },
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    assert_eq!(admitted.load(Ordering::SeqCst), 1);
    assert_eq!(report.summary.selected, 1);
    assert_eq!(report.summary.passed, 0);
    assert_eq!(report.summary.infrastructure_errors, 1);
    assert_eq!(report.cases[0].status, BenchCaseStatus::ArtifactError);
    assert_eq!(report.cases[0].tool_calls, Some(2));
    assert_eq!(report.cases[0].artifacts.len(), 1);
    BenchReport::verify(root).unwrap();
}
#[test]
fn utf8_snapshot_patch_can_be_applied_and_represents_full_changed_text() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("value.txt");
    std::fs::write(&file, "old\nsecond\n").unwrap();
    let before = WorkspaceSnapshot::capture(root.path()).unwrap();
    std::fs::write(&file, "new 汉字\nsecond\n").unwrap();
    let after = WorkspaceSnapshot::capture(root.path()).unwrap();
    let patch = before.unified_patch(&after).unwrap().unwrap();
    assert!(patch.contains("-old") && patch.contains("+new 汉字"));
    std::fs::write(&file, "old\nsecond\n").unwrap();
    let patch_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(patch_file.path(), patch).unwrap();
    let applied = std::process::Command::new("git")
        .arg("-C")
        .arg(root.path())
        .arg("apply")
        .arg(patch_file.path())
        .output()
        .unwrap();
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), "new 汉字\nsecond\n");
}
#[tokio::test]
async fn verifier_cross_checks_report_metrics_and_graders_even_after_refingerprinting() {
    use ring::digest::{SHA256, digest};
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    run(&root, &["retry_config"], ScriptVariant::Correct).await;
    let path = root.join("cases/retry_config-0/report.json");
    let original = std::fs::read(&path).unwrap();
    let manifest_original = manifest(&root);
    let mut report: Value = serde_json::from_slice(&original).unwrap();
    report["progress"]["tool_calls"] = json!(999);
    let bytes = serde_json::to_vec(&report).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let mut modified = manifest_original.clone();
    modified["cases"][0]["tool_calls"] = json!(999);
    modified["summary"]["total_tool_calls"] = json!(999);
    modified["cases"][0]["artifacts"][0]["bytes"] = json!(bytes.len());
    modified["cases"][0]["artifacts"][0]["sha256"] = json!(
        digest(&SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    save_manifest(&root, &modified);
    assert!(BenchReport::verify(&root).is_err());
    let mut report: Value = serde_json::from_slice(&original).unwrap();
    report["evaluations"][0]["verdict"] = json!("failed");
    let bytes = serde_json::to_vec(&report).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let mut modified = manifest_original;
    modified["cases"][0]["artifacts"][0]["bytes"] = json!(bytes.len());
    modified["cases"][0]["artifacts"][0]["sha256"] = json!(
        digest(&SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    save_manifest(&root, &modified);
    assert!(BenchReport::verify(&root).is_err());
}
#[tokio::test]
async fn deep_unreplayable_data_is_an_artifact_failure_without_repeating_execution() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("run");
    let mut suite = CodingSuite::builtin().unwrap();
    let mut nested = json!(true);
    for _ in 0..126 {
        nested = json!({"nested":nested});
    }
    // This host-only fixture exercises a response that cannot fit the archive's serde framing.
    // Initial fixture parsing/plan validation remains normal; generate the deep response in native code.
    struct Deep {
        value: Value,
        called: std::sync::atomic::AtomicBool,
    }
    #[async_trait]
    impl ModelProvider for Deep {
        fn model_spec(&self, name: &str) -> Result<ModelSpec, ProviderError> {
            let mut spec = ModelSpec::new("deep", name);
            spec.capabilities.tools = true;
            Ok(spec)
        }
        async fn generate(
            &self,
            request: ModelRequest,
            sink: Arc<dyn ModelEventSink>,
            _: TurnExecutionContext,
        ) -> Result<ModelResponse, ProviderError> {
            use std::sync::atomic::Ordering;
            let first = !self.called.swap(true, Ordering::SeqCst);
            let response = ModelResponse {
                id: None,
                model: request.model,
                message: if first {
                    Message {
                        role: MessageRole::Assistant,
                        content: vec![ContentBlock::ToolCall {
                            call: ModelToolCall {
                                id: "deep".into(),
                                name: "unknown".into(),
                                arguments: self.value.clone(),
                            },
                        }],
                    }
                } else {
                    Message::text(MessageRole::Assistant, "done")
                },
                stop_reason: if first {
                    StopReason::ToolCalls
                } else {
                    StopReason::Complete
                },
                usage: ModelUsage::default(),
            };
            emit_response(&response, sink.as_ref());
            Ok(response)
        }
    }
    suite.cases.retain(|c| c.id == "retry_config");
    let called = std::sync::atomic::AtomicUsize::new(0);
    let report = BenchRunner::new(suite, options(&root, &[]))
        .unwrap()
        .run(
            |c| c.request("offline", ModelOptions::default(), None),
            |_, w| {
                called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                TurnExecutor::builder(Deep {
                    value: nested.clone(),
                    called: std::sync::atomic::AtomicBool::new(false),
                })
                .workspace(w)
                .build()
            },
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(called.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(report.summary.passed, 0);
    assert_eq!(report.cases[0].status, BenchCaseStatus::ArtifactError);
    BenchReport::verify(root).unwrap();
}
#[tokio::test]
async fn equal_tool_schemas_with_different_host_capabilities_are_not_comparable() {
    use aicoder_core::tools::*;
    struct ClassifiedRead(ToolCapability);
    #[async_trait]
    impl ExecutableTool for ClassifiedRead {
        fn definition(&self) -> ToolDefinition {
            aicoder_runtime::tools::ReadFileTool.definition()
        }
        fn capability(&self) -> ToolCapability {
            self.0
        }
        async fn execute(
            &self,
            context: &ToolContext,
            args: Value,
        ) -> Result<ToolSuccess, ToolFailure> {
            aicoder_runtime::tools::ReadFileTool
                .execute(context, args)
                .await
        }
    }
    let parent = tempfile::tempdir().unwrap();
    let mut reports = Vec::new();
    for (name, capability) in [
        ("read", ToolCapability::ReadOnly),
        ("command", ToolCapability::Command),
    ] {
        let root = parent.path().join(name);
        let report = BenchRunner::new(
            CodingSuite::builtin().unwrap(),
            options(&root, &["retry_config"]),
        )
        .unwrap()
        .run(
            |c| c.request("offline", ModelOptions::default(), None),
            |case, w| {
                let mut registry = ToolRegistry::default();
                registry.register(ClassifiedRead(capability))?;
                registry.register(aicoder_runtime::tools::WriteFileTool)?;
                TurnExecutor::builder(ScriptedCodingProvider::new(case, ScriptVariant::Correct))
                    .workspace(w)
                    .registry(registry)
                    .approval(AllowAllApproval)
                    .build()
            },
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
        assert_eq!(report.summary.passed, 1);
        BenchReport::verify(root).unwrap();
        reports.push(report);
    }
    assert!(BenchComparison::compare(&reports[0], &reports[1]).is_err());
}
