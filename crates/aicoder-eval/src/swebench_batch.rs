use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use aicoder_core::AgentLoop;
use anyhow::{Context, Result, ensure};
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};

use crate::{
    EvalReport, EvalRunner, SweBenchAdapter, SweBenchCase, SweBenchPrediction,
    write_swebench_predictions,
};

#[derive(Debug, Clone)]
pub struct SweBenchBatchOptions {
    pub output_dir: PathBuf,
    pub run_id: String,
    pub dataset: String,
    pub concurrency: usize,
    pub resume: bool,
    pub parameters: serde_json::Value,
}

impl SweBenchBatchOptions {
    pub fn new(output_dir: impl Into<PathBuf>, run_id: impl Into<String>) -> Self {
        Self {
            output_dir: output_dir.into(),
            run_id: run_id.into(),
            dataset: "swe-bench".to_string(),
            concurrency: 1,
            resume: true,
            parameters: serde_json::Value::Null,
        }
    }

    pub fn dataset(mut self, dataset: impl Into<String>) -> Self {
        self.dataset = dataset.into();
        self
    }

    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency;
        self
    }

    pub fn resume(mut self, resume: bool) -> Self {
        self.resume = resume;
        self
    }

    pub fn parameters(mut self, parameters: serde_json::Value) -> Self {
        self.parameters = parameters;
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SweBenchBatchCaseStatus {
    Completed,
    Incomplete,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweBenchBatchCase {
    pub instance_id: String,
    pub status: SweBenchBatchCaseStatus,
    pub resumed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SweBenchBatchSummary {
    pub selected: usize,
    pub completed: usize,
    pub incomplete: usize,
    pub resumed: usize,
    pub failed: usize,
    pub predictions: usize,
    pub empty_patches: usize,
    pub total_tokens: i64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweBenchBatchReport {
    pub schema_version: u32,
    pub run_id: String,
    pub dataset: String,
    pub model: String,
    pub model_name_or_path: String,
    pub started_at_unix_seconds: u64,
    pub concurrency: usize,
    pub resume: bool,
    pub parameters: serde_json::Value,
    pub predictions_path: PathBuf,
    pub cases: Vec<SweBenchBatchCase>,
    pub summary: SweBenchBatchSummary,
}

pub struct SweBenchBatchRunner {
    adapter: SweBenchAdapter,
    eval_runner: EvalRunner,
    options: SweBenchBatchOptions,
}

impl SweBenchBatchRunner {
    pub fn new(adapter: SweBenchAdapter, options: SweBenchBatchOptions) -> Self {
        Self {
            adapter,
            eval_runner: EvalRunner::new(),
            options,
        }
    }

    pub fn eval_runner(mut self, runner: EvalRunner) -> Self {
        self.eval_runner = runner;
        self
    }

    pub async fn run<F>(
        &self,
        cases: Vec<SweBenchCase>,
        agent_factory: F,
    ) -> Result<SweBenchBatchReport>
    where
        F: Fn(&Path) -> Result<AgentLoop> + Send + Sync + 'static,
    {
        ensure!(
            self.options.concurrency > 0,
            "SWE-bench concurrency must be positive"
        );
        ensure!(
            !self.options.run_id.trim().is_empty(),
            "SWE-bench run id cannot be empty"
        );
        ensure!(!cases.is_empty(), "No SWE-bench cases selected");
        ensure_unique_artifact_names(&cases)?;
        fs::create_dir_all(self.options.output_dir.join("cases")).with_context(|| {
            format!(
                "Failed to create SWE-bench output directory {}",
                self.options.output_dir.display()
            )
        })?;

        let started_at_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let started = Instant::now();
        let factory = Arc::new(agent_factory);
        let adapter = Arc::new(self.adapter.clone());
        let runner = Arc::new(self.eval_runner.clone());
        let options = Arc::new(self.options.clone());
        let mut results = stream::iter(cases.into_iter().map(|case| {
            let factory = Arc::clone(&factory);
            let adapter = Arc::clone(&adapter);
            let runner = Arc::clone(&runner);
            let options = Arc::clone(&options);
            async move { run_case(case, adapter, runner, options, factory).await }
        }))
        .buffer_unordered(self.options.concurrency)
        .collect::<Vec<_>>()
        .await;
        results.sort_by(|left, right| left.instance_id.cmp(&right.instance_id));

        let mut predictions = results
            .iter()
            .filter_map(|result| result.prediction.clone())
            .collect::<Vec<_>>();
        predictions.sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
        let predictions_path = self.options.output_dir.join("predictions.json");
        write_predictions_atomic(&predictions_path, &predictions)?;

        let mut summary = SweBenchBatchSummary {
            selected: results.len(),
            predictions: predictions.len(),
            duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            ..SweBenchBatchSummary::default()
        };
        for result in &results {
            match result.case.status {
                SweBenchBatchCaseStatus::Completed => summary.completed += 1,
                SweBenchBatchCaseStatus::Incomplete => summary.incomplete += 1,
                SweBenchBatchCaseStatus::Failed => summary.failed += 1,
            }
            if result.case.resumed {
                summary.resumed += 1;
            }
            if let Some(checkpoint) = &result.checkpoint {
                summary.total_tokens += i64::from(checkpoint.report.run.usage.total_tokens);
                if checkpoint.prediction.model_patch.is_empty() {
                    summary.empty_patches += 1;
                }
            }
        }

        let report = SweBenchBatchReport {
            schema_version: 1,
            run_id: self.options.run_id.clone(),
            dataset: self.options.dataset.clone(),
            model: self.adapter.model().to_string(),
            model_name_or_path: self.adapter.model_name().to_string(),
            started_at_unix_seconds,
            concurrency: self.options.concurrency,
            resume: self.options.resume,
            parameters: self.options.parameters.clone(),
            predictions_path: predictions_path.clone(),
            cases: results.into_iter().map(|result| result.case).collect(),
            summary,
        };
        write_json_atomic(&self.options.output_dir.join("run.json"), &report)?;
        Ok(report)
    }
}

struct CaseRunResult {
    instance_id: String,
    case: SweBenchBatchCase,
    checkpoint: Option<CaseCheckpoint>,
    prediction: Option<SweBenchPrediction>,
}

#[derive(Serialize, Deserialize)]
struct CaseCheckpoint {
    run_id: String,
    dataset: String,
    model: String,
    model_name_or_path: String,
    parameters: serde_json::Value,
    report: EvalReport,
    prediction: SweBenchPrediction,
}

impl CaseCheckpoint {
    fn matches(
        &self,
        case: &SweBenchCase,
        adapter: &SweBenchAdapter,
        options: &SweBenchBatchOptions,
    ) -> bool {
        self.run_id == options.run_id
            && self.dataset == options.dataset
            && self.model == adapter.model()
            && self.model_name_or_path == adapter.model_name()
            && self.parameters == options.parameters
            && self.report.case.id == case.instance.instance_id
            && self.report.case.model == case.eval_case.request.model
            && self.prediction.instance_id == case.instance.instance_id
    }
}

async fn run_case<F>(
    case: SweBenchCase,
    adapter: Arc<SweBenchAdapter>,
    runner: Arc<EvalRunner>,
    options: Arc<SweBenchBatchOptions>,
    factory: Arc<F>,
) -> CaseRunResult
where
    F: Fn(&Path) -> Result<AgentLoop> + Send + Sync + 'static,
{
    let instance_id = case.instance.instance_id.clone();
    let case_dir = options
        .output_dir
        .join("cases")
        .join(artifact_name(&instance_id));
    let checkpoint_path = case_dir.join("checkpoint.json");
    let report_path = case_dir.join("report.json");
    let trace_path = case_dir.join("trace.json");
    let error_path = case_dir.join("error.json");
    if options.resume && checkpoint_path.is_file() {
        match read_json::<CaseCheckpoint>(&checkpoint_path) {
            Ok(checkpoint) if checkpoint.matches(&case, &adapter, &options) => {
                return successful_case(instance_id, true, report_path, trace_path, checkpoint);
            }
            Ok(_) => tracing::info!(
                instance_id,
                "Ignoring SWE-bench checkpoint from a different run configuration"
            ),
            Err(error) => {
                tracing::warn!(
                    instance_id,
                    error = %error,
                    "Ignoring invalid SWE-bench checkpoint"
                );
            }
        }
    }
    let execution = async {
        for stale in [&checkpoint_path, &report_path, &trace_path, &error_path] {
            remove_file_if_exists(stale)?;
        }
        fs::create_dir_all(&case_dir)
            .with_context(|| format!("Failed to create case directory {}", case_dir.display()))?;
        let report = runner.run(&case.eval_case, factory.as_ref()).await?;
        let prediction = adapter.prediction(&case, &report)?;
        write_json_atomic(&report_path, &report)?;
        write_trace_atomic(&trace_path, &report)?;
        let checkpoint = CaseCheckpoint {
            run_id: options.run_id.clone(),
            dataset: options.dataset.clone(),
            model: adapter.model().to_string(),
            model_name_or_path: adapter.model_name().to_string(),
            parameters: options.parameters.clone(),
            report,
            prediction,
        };
        write_json_atomic(&checkpoint_path, &checkpoint)?;
        Ok::<_, anyhow::Error>(checkpoint)
    }
    .await;

    match execution {
        Ok(checkpoint) => successful_case(instance_id, false, report_path, trace_path, checkpoint),
        Err(error) => {
            let message = format!("{error:#}");
            let _ = fs::create_dir_all(&case_dir);
            let _ = write_json_atomic(
                &error_path,
                &serde_json::json!({"instance_id": instance_id, "error": message}),
            );
            CaseRunResult {
                instance_id: instance_id.clone(),
                case: SweBenchBatchCase {
                    instance_id,
                    status: SweBenchBatchCaseStatus::Failed,
                    resumed: false,
                    report_path: None,
                    trace_path: None,
                    error: Some(message),
                },
                checkpoint: None,
                prediction: None,
            }
        }
    }
}

fn successful_case(
    instance_id: String,
    resumed: bool,
    report_path: PathBuf,
    trace_path: PathBuf,
    checkpoint: CaseCheckpoint,
) -> CaseRunResult {
    let status = match checkpoint.report.run.outcome {
        crate::EvalRunOutcome::Completed => SweBenchBatchCaseStatus::Completed,
        crate::EvalRunOutcome::Incomplete => SweBenchBatchCaseStatus::Incomplete,
        crate::EvalRunOutcome::Failed => SweBenchBatchCaseStatus::Failed,
    };
    let error = checkpoint.report.run.error.clone();
    CaseRunResult {
        instance_id: instance_id.clone(),
        case: SweBenchBatchCase {
            instance_id,
            status,
            resumed,
            report_path: Some(report_path),
            trace_path: Some(trace_path),
            error,
        },
        prediction: Some(checkpoint.prediction.clone()),
        checkpoint: Some(checkpoint),
    }
}

fn write_trace_atomic(path: &Path, report: &EvalReport) -> Result<()> {
    let temporary = temporary_path(path);
    report.trace.write_json(&temporary)?;
    fs::rename(&temporary, path)
        .with_context(|| format!("Failed to publish event trace {}", path.display()))
}

fn write_predictions_atomic(path: &Path, predictions: &[SweBenchPrediction]) -> Result<()> {
    let temporary = temporary_path(path);
    write_swebench_predictions(&temporary, predictions)?;
    fs::rename(&temporary, path)
        .with_context(|| format!("Failed to publish predictions {}", path.display()))
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = temporary_path(path);
    let encoded = serde_json::to_vec_pretty(value).context("Failed to encode JSON artifact")?;
    fs::write(&temporary, encoded)
        .with_context(|| format!("Failed to write temporary artifact {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .with_context(|| format!("Failed to publish artifact {}", path.display()))
}

fn read_json<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let content =
        fs::read(path).with_context(|| format!("Failed to read checkpoint {}", path.display()))?;
    serde_json::from_slice(&content)
        .with_context(|| format!("Failed to parse checkpoint {}", path.display()))
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension(format!("{}.tmp", std::process::id()))
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("Failed to remove stale artifact {}", path.display())),
    }
}

fn ensure_unique_artifact_names(cases: &[SweBenchCase]) -> Result<()> {
    let mut names = HashSet::new();
    for case in cases {
        let name = artifact_name(&case.instance.instance_id);
        ensure!(
            names.insert(name),
            "SWE-bench instance ids collide as artifact paths: {}",
            case.instance.instance_id
        );
    }
    Ok(())
}

fn artifact_name(instance_id: &str) -> String {
    instance_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}
