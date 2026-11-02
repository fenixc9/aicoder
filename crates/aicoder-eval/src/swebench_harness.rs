use crate::{
    SweBenchBatchCaseStatus, SweBenchBatchReport, SweBenchDataset, SweBenchPrediction,
    storage::{OutputRef, StorageRoot},
};
use aicoder_core::{TurnExecutionContext, tools::ToolContext};
use aicoder_runtime::{
    process::{ProcessCaptureOptions, capture},
    tools::{ShellLimits, ShellOutput, ShellStopReason},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
pub const SWEBENCH_HARNESS_VERSION: &str = "5.0.2";
pub const SWEBENCH_HARNESS_PACKAGE_SHA256: &str =
    "b658165e24eb8c4a27d12586a68593ff27c4f8b3794a41991870b2000f726cc5";
#[derive(Debug, Clone)]
pub struct SweBenchHarnessConfig {
    pub python: PathBuf,
    pub dataset_name: String,
    pub split: String,
    pub predictions_path: PathBuf,
    pub instance_ids: Vec<String>,
    pub max_workers: usize,
    pub run_id: String,
    pub timeout_seconds: u64,
    pub report_dir: PathBuf,
    pub modal: bool,
    pub generation_manifest: Option<PathBuf>,
    pub total_timeout_seconds: u64,
    pub excluded_environment: Vec<String>,
}
impl SweBenchHarnessConfig {
    pub fn new(
        dataset_name: impl Into<String>,
        predictions_path: impl Into<PathBuf>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            python: "python".into(),
            dataset_name: dataset_name.into(),
            split: "test".into(),
            predictions_path: predictions_path.into(),
            instance_ids: Vec::new(),
            max_workers: 1,
            run_id: run_id.into(),
            timeout_seconds: 1800,
            report_dir: ".".into(),
            modal: false,
            generation_manifest: None,
            total_timeout_seconds: 7200,
            excluded_environment: Vec::new(),
        }
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            (1..=16).contains(&self.max_workers)
                && (1..=86400).contains(&self.total_timeout_seconds)
                && (1..=86400).contains(&self.timeout_seconds),
            "invalid harness worker/deadline settings"
        );
        ensure!(
            crate::swebench::valid_run_id(&self.run_id),
            "invalid harness run id"
        );
        ensure!(
            !self.split.is_empty() && self.split.len() <= 64 && self.dataset_name.len() <= 4096,
            "invalid grading dataset/split"
        );
        ensure!(
            !self.modal,
            "this pinned local harness adapter does not submit work to Modal"
        );
        Ok(())
    }
    /// Build native argv only. The runner separately pins installed package contents and inputs.
    pub fn command(&self) -> Result<Command> {
        self.validate()?;
        let mut command = Command::new(resolve_python(&self.python)?);
        command
            .current_dir(&self.report_dir)
            .args([
                "-I",
                "-m",
                "swebench.harness.run_evaluation",
                "--dataset_name",
            ])
            .arg(&self.dataset_name)
            .args(["--split", &self.split, "--predictions_path"])
            .arg(&self.predictions_path)
            .arg("--max_workers")
            .arg(self.max_workers.to_string())
            .arg("--run_id")
            .arg(&self.run_id)
            .arg("--timeout")
            .arg(self.timeout_seconds.to_string())
            .args(["--report_dir", ".", "--modal", "false"]);
        if !self.instance_ids.is_empty() {
            command.arg("--instance_ids").args(&self.instance_ids);
        }
        Ok(command)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SweBenchHarnessReport {
    pub total_instances: usize,
    pub submitted_instances: usize,
    pub completed_instances: usize,
    pub resolved_instances: usize,
    pub unresolved_instances: usize,
    pub empty_patch_instances: usize,
    pub error_instances: usize,
    #[serde(default)]
    pub completed_ids: Vec<String>,
    #[serde(default)]
    pub incomplete_ids: Vec<String>,
    #[serde(default)]
    pub empty_patch_ids: Vec<String>,
    #[serde(default)]
    pub submitted_ids: Vec<String>,
    #[serde(default)]
    pub resolved_ids: Vec<String>,
    #[serde(default)]
    pub unresolved_ids: Vec<String>,
    #[serde(default)]
    pub error_ids: Vec<String>,
    #[serde(default)]
    pub schema_version: u32,
}
impl SweBenchHarnessReport {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let report: Self =
            serde_json::from_slice(&crate::bounds::read(path.as_ref(), 4 * 1024 * 1024)?)?;
        report.validate()?;
        Ok(report)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 2 && self.total_instances <= 20_000,
            "unsupported harness report version/count"
        );
        for (count, ids) in [
            (self.completed_instances, &self.completed_ids),
            (self.resolved_instances, &self.resolved_ids),
            (self.unresolved_instances, &self.unresolved_ids),
            (self.empty_patch_instances, &self.empty_patch_ids),
            (self.error_instances, &self.error_ids),
            (self.submitted_instances, &self.submitted_ids),
        ] {
            ensure!(
                ids.len() == count
                    && ids.len() <= 20_000
                    && ids.iter().all(|id| crate::swebench::valid_instance_id(id))
                    && ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
                "harness counts/ids differ"
            );
        }
        ensure!(
            self.resolved_instances <= self.total_instances,
            "harness resolved count exceeds denominator"
        );
        let completed = self.completed_ids.iter().collect::<BTreeSet<_>>();
        let resolved = self.resolved_ids.iter().collect::<BTreeSet<_>>();
        let unresolved = self.unresolved_ids.iter().collect::<BTreeSet<_>>();
        let errors = self.error_ids.iter().collect::<BTreeSet<_>>();
        let empty = self.empty_patch_ids.iter().collect::<BTreeSet<_>>();
        let submitted = self.submitted_ids.iter().collect::<BTreeSet<_>>();
        ensure!(
            resolved.is_subset(&completed)
                && unresolved.is_subset(&completed)
                && resolved.is_disjoint(&unresolved)
                && resolved.is_disjoint(&errors)
                && unresolved.is_disjoint(&errors)
                && empty.is_disjoint(&completed)
                && empty.is_disjoint(&errors),
            "harness outcome sets conflict"
        );
        ensure!(
            completed.is_subset(&submitted)
                && errors.is_subset(&submitted)
                && empty.is_subset(&submitted),
            "harness reports an unsubmitted outcome"
        );
        Ok(())
    }
    /// Dataset/explicit-selection denominator, never only successfully generated submissions.
    pub fn resolved_rate(&self) -> f64 {
        if self.total_instances == 0 {
            0.0
        } else {
            self.resolved_instances as f64 / self.total_instances as f64
        }
    }
    pub fn submitted_resolved_rate(&self) -> f64 {
        if self.submitted_instances == 0 {
            0.0
        } else {
            self.resolved_instances as f64 / self.submitted_instances as f64
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SweBenchHarnessIdentity {
    pub version: String,
    pub package_sha256: String,
    pub files: usize,
    pub python: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweBenchScore {
    pub schema_version: u32,
    pub logical_run_id: String,
    pub harness_run_id: String,
    pub selected_ids: Vec<String>,
    pub selected: usize,
    pub generation_selected: Option<usize>,
    pub generation_failures: Vec<String>,
    pub predictions_sha256: String,
    pub dataset_sha256: Option<String>,
    pub generation_sha256: Option<String>,
    pub resolved: usize,
    pub resolved_rate: f64,
    pub scope: String,
    pub harness: SweBenchHarnessIdentity,
    pub official: SweBenchHarnessReport,
}
#[derive(Debug)]
pub struct SweBenchHarnessExecution {
    pub output: Output,
    pub report_path: Option<PathBuf>,
    pub report: Option<SweBenchHarnessReport>,
    pub capture: Option<ShellOutput>,
    pub score: SweBenchScore,
    pub resumed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct GradePlan {
    schema_version: u32,
    logical_run_id: String,
    harness_run_id: String,
    harness: SweBenchHarnessIdentity,
    dataset_name: String,
    split: String,
    selection: Vec<String>,
    predictions_sha256: String,
    dataset_sha256: Option<String>,
    generation_sha256: Option<String>,
    workers: usize,
    case_timeout_seconds: u64,
    total_timeout_seconds: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GradeReceipt {
    schema_version: u32,
    plan_sha256: String,
    official: OutputRef,
    capture: OutputRef,
    score: OutputRef,
}
fn read_reference(root: &StorageRoot, reference: &OutputRef) -> Result<Vec<u8>> {
    ensure!(
        reference.bytes <= 4 * 1024 * 1024,
        "grading reference budget exceeded"
    );
    let bytes = root.read(&reference.path, reference.bytes)?;
    ensure!(
        bytes.len() == reference.bytes && crate::bounds::sha256(&bytes) == reference.sha256,
        "grading artifact fingerprint mismatch"
    );
    Ok(bytes)
}
fn cached_score(root: &StorageRoot, plan: &GradePlan) -> Result<SweBenchScore> {
    let receipt: GradeReceipt =
        serde_json::from_slice(&root.read("grade-receipt.json", 1024 * 1024)?)?;
    ensure!(
        receipt.schema_version == 1
            && receipt.plan_sha256
                == crate::bounds::sha256(&crate::bounds::encode(plan, 1024 * 1024)?),
        "grading receipt plan mismatch"
    );
    ensure!(
        receipt.score.path == "score.json"
            && receipt.capture.path == "capture.json"
            && receipt.official.path
                == format!("{}.{}.json", read_model_label(root)?, plan.harness_run_id),
        "foreign grading artifact reference"
    );
    let score: SweBenchScore = serde_json::from_slice(&read_reference(root, &receipt.score)?)?;
    let official: SweBenchHarnessReport =
        serde_json::from_slice(&read_reference(root, &receipt.official)?)?;
    let capture: ShellOutput = serde_json::from_slice(&read_reference(root, &receipt.capture)?)?;
    ensure!(
        score.official == official && successful(&capture),
        "grading receipt/capture incomplete"
    );
    ensure!(
        crate::bounds::sha256(&root.read("predictions.input.json", 256 * 1024 * 1024)?)
            == plan.predictions_sha256,
        "cached grading prediction snapshot changed"
    );
    if let Some(hash) = &plan.dataset_sha256 {
        ensure!(
            crate::bounds::sha256(&root.read("dataset.input.json", 256 * 1024 * 1024)?) == *hash,
            "cached grading dataset snapshot changed"
        );
    }
    validate_score(&score, plan)?;
    Ok(score)
}
fn read_model_label(root: &StorageRoot) -> Result<String> {
    let rows: Vec<SweBenchPrediction> =
        serde_json::from_slice(&root.read("predictions.input.json", 256 * 1024 * 1024)?)?;
    let first = rows
        .first()
        .ok_or_else(|| anyhow::anyhow!("cached prediction table empty"))?;
    ensure!(
        crate::swebench::valid_model_label(&first.model_name_or_path),
        "invalid cached model label"
    );
    Ok(first.model_name_or_path.replace('/', "__"))
}
impl SweBenchScore {
    /// Verify completed grading data without importing Python or launching a container.
    pub fn load_verified(directory: impl AsRef<Path>) -> Result<Self> {
        let root = StorageRoot::existing(directory.as_ref())?;
        let _lease = root.lease()?;
        let plan: GradePlan = serde_json::from_slice(&root.read("grade-plan.json", 1024 * 1024)?)?;
        ensure!(
            plan.schema_version == 1
                && plan.harness.version == SWEBENCH_HARNESS_VERSION
                && plan.harness.package_sha256 == SWEBENCH_HARNESS_PACKAGE_SHA256,
            "unsupported grading identity"
        );
        cached_score(&root, &plan)
    }
}
/// Compatibility wrapper. New async hosts use the context-aware entry to cancel and await owners.
pub fn run_swebench_harness(config: &SweBenchHarnessConfig) -> Result<SweBenchHarnessExecution> {
    let config = config.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(run_swebench_harness_with_context(
                &config,
                TurnExecutionContext::new(),
            ))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("harness owner panicked"))?
}
fn resolve_python(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() || path.components().count() == 1 {
        Ok(path.into())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
async fn process(
    command: Command,
    cwd: &Path,
    seconds: u64,
    scope: TurnExecutionContext,
    excluded: &[String],
    scan: u64,
) -> Result<ShellOutput> {
    let context = ToolContext::new(cwd, Duration::from_secs(seconds), 65536)?;
    let mut options = ProcessCaptureOptions::default();
    options.limits = ShellLimits {
        max_command_bytes: 1024 * 1024,
        max_output_bytes: scan,
    };
    options.excluded_environment = excluded.into();
    capture(command, &context, options, scope).await
}
async fn cleanup_owned(
    config: &SweBenchHarnessConfig,
    plan: &GradePlan,
    directory: &Path,
) -> Result<serde_json::Value> {
    let names = plan
        .selection
        .iter()
        .map(|id| format!("sweb.eval.{}.{}", id.to_lowercase(), plan.harness_run_id))
        .collect::<Vec<_>>();
    let mut command = Command::new(resolve_python(&config.python)?);
    command
        .args(["-I", "-c", include_str!("harness_cleanup.py")])
        .arg(serde_json::to_string(&names)?);
    let output = process(
        command,
        directory,
        30,
        TurnExecutionContext::new(),
        &config.excluded_environment,
        65536,
    )
    .await?;
    ensure!(
        successful(&output) && !output.truncated,
        "owned container cleanup could not be confirmed"
    );
    let value: serde_json::Value = serde_json::from_str(&output.stdout)?;
    ensure!(
        value["remaining"].as_array().is_some_and(|v| v.is_empty())
            && value["errors"].as_array().is_some_and(|v| v.is_empty()),
        "owned container cleanup incomplete"
    );
    Ok(value)
}
fn successful(output: &ShellOutput) -> bool {
    matches!(output.stop_reason, ShellStopReason::Exit)
        && output.exit_code == Some(0)
        && output.reaped
        && output.cleanup_ok
        && output.output_complete
}
async fn identity(
    config: &SweBenchHarnessConfig,
    scope: TurnExecutionContext,
) -> Result<SweBenchHarnessIdentity> {
    let temporary = tempfile::tempdir()?;
    let mut probe = Command::new(resolve_python(&config.python)?);
    probe.args(["-I", "-c", include_str!("harness_probe.py")]);
    let output = process(
        probe,
        temporary.path(),
        30,
        scope,
        &config.excluded_environment,
        65536,
    )
    .await?;
    ensure!(
        successful(&output) && !output.truncated,
        "harness identity probe failed or was incomplete"
    );
    let identity: SweBenchHarnessIdentity = serde_json::from_str(&output.stdout)?;
    ensure!(
        identity.version == SWEBENCH_HARNESS_VERSION
            && identity.package_sha256 == SWEBENCH_HARNESS_PACKAGE_SHA256
            && identity.files == 65,
        "unsupported or modified harness; install the pinned swebench==5.0.2 wheel"
    );
    Ok(identity)
}
fn predictions(path: &Path) -> Result<Vec<SweBenchPrediction>> {
    let bytes = crate::bounds::read(path, 256 * 1024 * 1024)?;
    let predictions: Vec<SweBenchPrediction> = serde_json::from_slice(&bytes)?;
    ensure!(
        !predictions.is_empty() && predictions.len() <= 5000,
        "prediction count budget exceeded"
    );
    let mut ids = BTreeSet::new();
    let label = &predictions[0].model_name_or_path;
    ensure!(
        crate::swebench::valid_model_label(label),
        "invalid prediction model label"
    );
    for prediction in &predictions {
        ensure!(
            crate::swebench::valid_instance_id(&prediction.instance_id)
                && ids.insert(&prediction.instance_id)
                && &prediction.model_name_or_path == label
                && prediction.model_patch.len() <= 8 * 1024 * 1024,
            "invalid, duplicate or mixed-model prediction"
        );
    }
    Ok(predictions)
}
pub async fn run_swebench_harness_with_context(
    config: &SweBenchHarnessConfig,
    scope: TurnExecutionContext,
) -> Result<SweBenchHarnessExecution> {
    config.validate()?;
    let all = predictions(&config.predictions_path)?;
    let selected = if config.instance_ids.is_empty() {
        all.iter()
            .map(|p| p.instance_id.clone())
            .collect::<BTreeSet<_>>()
    } else {
        let ids = config.instance_ids.iter().cloned().collect::<BTreeSet<_>>();
        ensure!(
            ids.len() == config.instance_ids.len()
                && ids
                    .iter()
                    .all(|id| all.iter().any(|p| &p.instance_id == id)),
            "unknown or duplicate grading selection"
        );
        ids
    };
    let scoped = all
        .iter()
        .filter(|p| selected.contains(&p.instance_id))
        .cloned()
        .collect::<Vec<_>>();
    let predictions_bytes = crate::bounds::encode(&scoped, 256 * 1024 * 1024)?;
    let predictions_sha256 = crate::bounds::sha256(&predictions_bytes);
    let generation = if let Some(path) = &config.generation_manifest {
        Some(SweBenchBatchReport::load_verified(path)?)
    } else {
        None
    };
    if let Some(generation) = &generation {
        ensure!(
            generation.model_name_or_path == all[0].model_name_or_path
                && generation.cases.len() == all.len()
                && generation
                    .cases
                    .iter()
                    .all(|c| all.iter().any(|p| p.instance_id == c.instance_id)),
            "grading predictions differ from generation selection"
        );
        let raw = crate::bounds::sha256(&crate::bounds::read(
            &config.predictions_path,
            256 * 1024 * 1024,
        )?);
        ensure!(
            raw == generation.predictions_sha256,
            "grading must use the verified generation prediction table"
        );
    }
    let local_dataset = Path::new(&config.dataset_name).is_file();
    ensure!(
        generation.is_none() || local_dataset,
        "linked generation grading requires the pinned local dataset file"
    );
    let dataset = if local_dataset {
        Some(SweBenchDataset::load(&config.dataset_name)?)
    } else {
        None
    };
    if let Some(dataset) = &dataset {
        ensure!(
            selected.iter().all(|id| dataset.get(id).is_some()),
            "grading dataset omits selected generation cases"
        );
    }
    if let (Some(generation), Some(dataset)) = (&generation, &dataset) {
        for case in &generation.cases {
            ensure!(
                dataset.get(&case.instance_id).is_some_and(
                    |row| row.fingerprint().ok().as_deref() == Some(&case.instance_sha256)
                ),
                "grading dataset row differs from generation pin"
            );
        }
    }
    let dataset_bytes = dataset
        .as_ref()
        .map(|d| {
            crate::bounds::encode(
                &d.instances
                    .iter()
                    .filter(|i| selected.contains(&i.instance_id))
                    .collect::<Vec<_>>(),
                256 * 1024 * 1024,
            )
        })
        .transpose()?;
    let dataset_sha256 = dataset_bytes
        .as_ref()
        .map(|bytes| crate::bounds::sha256(bytes));
    let generation_sha256 = generation
        .as_ref()
        .map(|g| {
            crate::bounds::encode(
                &json!({"plan_sha256":g.plan_sha256,"predictions_sha256":g.predictions_sha256}),
                1024,
            )
            .map(|bytes| crate::bounds::sha256(&bytes))
        })
        .transpose()?;
    let parent = config.report_dir.join("aicoder-swe-grading");
    fs::create_dir_all(&parent)?;
    let directory = parent.join(&config.run_id);
    let existing = directory.exists();
    let fresh_identity = if !existing {
        Some(identity(config, scope.clone()).await?)
    } else {
        None
    };
    let root = if existing {
        StorageRoot::existing(&directory)?
    } else {
        StorageRoot::create(&directory)?
    };
    let _lease = root.lease()?;
    let prior = if existing {
        Some(serde_json::from_slice::<GradePlan>(
            &root.read("grade-plan.json", 1024 * 1024)?,
        )?)
    } else {
        None
    };
    let harness = if let Some(prior) = &prior {
        ensure!(
            prior.harness.version == SWEBENCH_HARNESS_VERSION
                && prior.harness.package_sha256 == SWEBENCH_HARNESS_PACKAGE_SHA256
                && prior.harness.files == 65,
            "unsupported cached harness identity"
        );
        prior.harness.clone()
    } else {
        fresh_identity.expect("fresh identity was checked before namespace creation")
    };
    let mut plan = GradePlan {
        schema_version: 1,
        logical_run_id: config.run_id.clone(),
        harness_run_id: format!(
            "aicoder_{}_{}",
            config.run_id,
            &aicoder_core::events::RunId::new()
                .as_uuid()
                .simple()
                .to_string()[..12]
        ),
        harness: harness.clone(),
        dataset_name: config.dataset_name.clone(),
        split: config.split.clone(),
        selection: selected.iter().cloned().collect(),
        predictions_sha256: predictions_sha256.clone(),
        dataset_sha256: dataset_sha256.clone(),
        generation_sha256: generation_sha256.clone(),
        workers: config.max_workers,
        case_timeout_seconds: config.timeout_seconds,
        total_timeout_seconds: config.total_timeout_seconds,
    };
    if let Some(prior) = prior {
        plan.harness_run_id = prior.harness_run_id.clone();
        ensure!(
            plan == prior,
            "grading inputs changed; use a new run id, old cached results were retained"
        );
        ensure!(
            root.exists("grade-receipt.json")?,
            "an earlier harness may have executed without complete publication; no automatic rerun"
        );
        let score = cached_score(&root, &plan)?;
        let output = empty_capture();
        let filename = format!(
            "{}.{}.json",
            all[0].model_name_or_path.replace('/', "__"),
            plan.harness_run_id
        );
        return Ok(SweBenchHarnessExecution {
            output: std_output(&output),
            report_path: Some(directory.join(filename)),
            report: Some(score.official.clone()),
            capture: None,
            score,
            resumed: true,
        });
    }
    root.publish_json("grade-plan.json", &plan, 1024 * 1024, false)?;
    root.write("predictions.input.json", &predictions_bytes)?;
    if let Some(bytes) = &dataset_bytes {
        root.write("dataset.input.json", bytes)?;
    }
    let mut owned = config.clone();
    owned.run_id = plan.harness_run_id.clone();
    owned.report_dir = directory.canonicalize()?;
    owned.predictions_path = owned.report_dir.join("predictions.input.json");
    owned.instance_ids = plan.selection.clone();
    if dataset_bytes.is_some() {
        owned.dataset_name = owned
            .report_dir
            .join("dataset.input.json")
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("grading path must be UTF-8"))?
            .into();
    }
    root.publish_json(
        "started.json",
        &json!({"harness_run_id":plan.harness_run_id}),
        4096,
        false,
    )?;
    let capture = process(
        owned.command()?,
        &owned.report_dir,
        owned.total_timeout_seconds,
        scope,
        &owned.excluded_environment,
        32 * 1024 * 1024,
    )
    .await?;
    let capture_reference = root.publish_json("capture.json", &capture, 1024 * 1024, false)?;
    if !successful(&capture) {
        let cleanup = cleanup_owned(config, &plan, &owned.report_dir).await;
        match cleanup {
            Ok(value) => {
                root.publish_json("container-cleanup.json", &value, 65536, false)?;
            }
            Err(error) => {
                root.publish_json(
                    "container-cleanup.json",
                    &json!({"confirmed":false,"error":error.to_string()}),
                    65536,
                    false,
                )?;
                return Err(anyhow::anyhow!(
                    "harness {} and container cleanup was not confirmed; operations were not retried",
                    capture.stop_reason.name()
                ));
            }
        }
    }
    ensure!(
        successful(&capture),
        "harness stopped with {} or incomplete cleanup; operations were not retried",
        capture.stop_reason.name()
    );
    ensure!(
        crate::bounds::sha256(&root.read("predictions.input.json", 256 * 1024 * 1024)?)
            == plan.predictions_sha256,
        "grading prediction snapshot changed"
    );
    if let Some(hash) = &plan.dataset_sha256 {
        ensure!(
            crate::bounds::sha256(&root.read("dataset.input.json", 256 * 1024 * 1024)?) == *hash,
            "grading dataset snapshot changed"
        );
    }
    let filename = format!(
        "{}.{}.json",
        all[0].model_name_or_path.replace('/', "__"),
        plan.harness_run_id
    );
    let official_bytes = root.read(&filename, 4 * 1024 * 1024)?;
    let official_reference = OutputRef {
        path: filename.clone(),
        bytes: official_bytes.len(),
        sha256: crate::bounds::sha256(&official_bytes),
    };
    let official: SweBenchHarnessReport = serde_json::from_slice(&official_bytes)?;
    official.validate()?;
    let score = SweBenchScore {
        schema_version: 1,
        logical_run_id: config.run_id.clone(),
        harness_run_id: plan.harness_run_id.clone(),
        selected_ids: plan.selection.clone(),
        selected: selected.len(),
        generation_selected: generation.as_ref().map(|g| g.cases.len()),
        generation_failures: generation
            .as_ref()
            .map(|g| {
                g.cases
                    .iter()
                    .filter(|c| {
                        selected.contains(&c.instance_id)
                            && c.status != SweBenchBatchCaseStatus::Completed
                    })
                    .map(|c| c.instance_id.clone())
                    .collect()
            })
            .unwrap_or_default(),
        predictions_sha256,
        dataset_sha256,
        generation_sha256,
        resolved: official.resolved_instances,
        resolved_rate: official.resolved_rate(),
        scope: if generation.is_some() {
            if config.instance_ids.is_empty() {
                "all_generation_selected"
            } else {
                "explicit_generation_subset"
            }
        } else {
            "prediction_selection_without_generation_manifest"
        }
        .into(),
        harness,
        official,
    };
    validate_score(&score, &plan)?;
    let score_reference = root.publish_json("score.json", &score, 4 * 1024 * 1024, false)?;
    root.publish_json(
        "grade-receipt.json",
        &GradeReceipt {
            schema_version: 1,
            plan_sha256: crate::bounds::sha256(&crate::bounds::encode(&plan, 1024 * 1024)?),
            official: official_reference,
            capture: capture_reference,
            score: score_reference,
        },
        1024 * 1024,
        false,
    )?;
    Ok(SweBenchHarnessExecution {
        output: std_output(&capture),
        report_path: Some(directory.join(&filename)),
        report: Some(score.official.clone()),
        capture: Some(capture),
        score,
        resumed: false,
    })
}
fn validate_score(score: &SweBenchScore, plan: &GradePlan) -> Result<()> {
    score.official.validate()?;
    let selected = plan.selection.iter().collect::<BTreeSet<_>>();
    let submitted = score.official.submitted_ids.iter().collect::<BTreeSet<_>>();
    ensure!(
        score.schema_version == 1
            && score.logical_run_id == plan.logical_run_id
            && score.harness_run_id == plan.harness_run_id
            && score.harness == plan.harness
            && score.selected_ids == plan.selection
            && score.selected == selected.len()
            && score.predictions_sha256 == plan.predictions_sha256
            && score.dataset_sha256 == plan.dataset_sha256
            && score.generation_sha256 == plan.generation_sha256,
        "score input identity mismatch"
    );
    ensure!(
        score.official.total_instances == selected.len()
            && submitted == selected
            && score.official.incomplete_ids.is_empty(),
        "official grading silently changed the selected denominator"
    );
    let classified = score
        .official
        .resolved_ids
        .iter()
        .chain(&score.official.unresolved_ids)
        .chain(&score.official.empty_patch_ids)
        .chain(&score.official.error_ids)
        .collect::<BTreeSet<_>>();
    ensure!(
        classified == selected,
        "official grading leaves selected outcomes unclassified"
    );
    ensure!(
        score.resolved == score.official.resolved_instances
            && score.resolved_rate == score.official.resolved_rate(),
        "resolved rate differs from full selected denominator"
    );
    Ok(())
}
fn std_output(capture: &ShellOutput) -> Output {
    use std::os::unix::process::ExitStatusExt;
    Output {
        status: std::process::ExitStatus::from_raw(
            capture
                .signal
                .unwrap_or_else(|| capture.exit_code.unwrap_or(1) << 8),
        ),
        stdout: capture.stdout.as_bytes().into(),
        stderr: capture.stderr.as_bytes().into(),
    }
}
fn empty_capture() -> ShellOutput {
    serde_json::from_value(json!({"exit_code":0,"signal":null,"stdout":"","stderr":"","stdout_bytes":0,"stderr_bytes":0,"stdout_omitted_bytes":0,"stderr_omitted_bytes":0,"truncated":false,"output_complete":true,"stop_reason":"exit","process_group_signal_requested":false,"reaped":true,"cleanup_ok":true,"encoding":"utf8_lossy"})).expect("empty capture")
}
