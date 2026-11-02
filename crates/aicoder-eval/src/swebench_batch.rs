use crate::{
    EvalReport, EvalRunner, SweBenchAdapter, SweBenchCase, SweBenchPrediction, UsageSummary,
    storage::{OutputRef, StorageRoot},
};
use aicoder_core::{TurnExecutionContext, TurnExecutor};
use anyhow::{Result, ensure};
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
const RECORD_LIMIT: usize = 64 * 1024 * 1024;
const DISK_LIMIT: usize = 512 * 1024 * 1024;
#[derive(Debug, Clone)]
pub struct SweBenchBatchOptions {
    pub output_dir: PathBuf,
    pub run_id: String,
    pub dataset: String,
    pub concurrency: usize,
    pub resume: bool,
    pub parameters: Value,
    /// Host pin for actual agent source/build/configuration. None permits a fresh run, never reuse.
    pub execution_pin: Option<String>,
}
impl SweBenchBatchOptions {
    pub fn new(output_dir: impl Into<PathBuf>, run_id: impl Into<String>) -> Self {
        Self {
            output_dir: output_dir.into(),
            run_id: run_id.into(),
            dataset: "swe-bench".into(),
            concurrency: 1,
            resume: true,
            parameters: Value::Null,
            execution_pin: None,
        }
    }
    pub fn dataset(mut self, value: impl Into<String>) -> Self {
        self.dataset = value.into();
        self
    }
    pub fn concurrency(mut self, value: usize) -> Self {
        self.concurrency = value;
        self
    }
    pub fn resume(mut self, value: bool) -> Self {
        self.resume = value;
        self
    }
    pub fn parameters(mut self, value: Value) -> Self {
        self.parameters = value;
        self
    }
    pub fn execution_pin(mut self, value: impl Into<String>) -> Self {
        self.execution_pin = Some(value.into());
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
    #[serde(default)]
    pub input_sha256: String,
    #[serde(default)]
    pub instance_sha256: String,
    #[serde(default)]
    pub artifacts: Vec<OutputRef>,
    #[serde(default)]
    pub usage: UsageSummary,
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
    pub total_tokens: Option<u64>,
    pub duration_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweBenchBatchReport {
    pub schema_version: u32,
    #[serde(default)]
    pub event_schema_version: u32,
    pub run_id: String,
    pub dataset: String,
    pub model: String,
    pub model_name_or_path: String,
    pub started_at_unix_seconds: u64,
    pub concurrency: usize,
    pub resume: bool,
    pub parameters: Value,
    pub predictions_path: PathBuf,
    pub cases: Vec<SweBenchBatchCase>,
    pub summary: SweBenchBatchSummary,
    #[serde(default)]
    pub plan_sha256: String,
    #[serde(default)]
    pub predictions_sha256: String,
    #[serde(default)]
    pub execution_pin: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct CaseInput {
    id: String,
    input_sha256: String,
    instance_sha256: String,
    request_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    run_id: String,
    dataset: String,
    model: String,
    model_name_or_path: String,
    concurrency: usize,
    parameters: Value,
    execution_pin: Option<String>,
    cases: Vec<CaseInput>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    plan_sha256: String,
    case: SweBenchBatchCase,
    prediction: SweBenchPrediction,
}
struct Done {
    case: SweBenchBatchCase,
    prediction: SweBenchPrediction,
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
    pub async fn run<F>(&self, cases: Vec<SweBenchCase>, factory: F) -> Result<SweBenchBatchReport>
    where
        F: Fn(&Path) -> Result<TurnExecutor> + Send + Sync + 'static,
    {
        self.run_with_context(cases, factory, TurnExecutionContext::new())
            .await
    }
    pub async fn run_with_context<F>(
        &self,
        mut cases: Vec<SweBenchCase>,
        factory: F,
        context: TurnExecutionContext,
    ) -> Result<SweBenchBatchReport>
    where
        F: Fn(&Path) -> Result<TurnExecutor> + Send + Sync + 'static,
    {
        ensure!(
            (1..=16).contains(&self.options.concurrency),
            "SWE-bench concurrency must be 1..16"
        );
        ensure!(
            crate::swebench::valid_run_id(&self.options.run_id),
            "invalid SWE-bench run id"
        );
        ensure!(
            !cases.is_empty() && cases.len() <= 5000,
            "SWE-bench selected case budget exceeded"
        );
        ensure!(
            self.options
                .execution_pin
                .as_ref()
                .is_none_or(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())),
            "invalid execution pin"
        );
        ensure!(
            crate::swebench::valid_model_label(self.adapter.model_name()),
            "invalid prediction model label"
        );
        crate::bounds::encoded_size(&self.options.parameters, 1024 * 1024)?;
        let mut ids = BTreeSet::new();
        for case in &cases {
            ensure!(
                crate::swebench::valid_instance_id(&case.instance.instance_id)
                    && ids.insert(&case.instance.instance_id),
                "invalid or duplicate SWE-bench instance id"
            );
            ensure!(
                case.eval_case.id == case.instance.instance_id,
                "case identity mismatch"
            );
        }
        cases.sort_by(|a, b| a.instance.instance_id.cmp(&b.instance.instance_id));
        let plan = Plan {
            schema_version: 4,
            run_id: self.options.run_id.clone(),
            dataset: self.options.dataset.clone(),
            model: self.adapter.model().into(),
            model_name_or_path: self.adapter.model_name().into(),
            concurrency: self.options.concurrency,
            parameters: self.options.parameters.clone(),
            execution_pin: self.options.execution_pin.clone(),
            cases: cases
                .iter()
                .map(|case| {
                    Ok(CaseInput {
                        id: case.instance.instance_id.clone(),
                        input_sha256: case.input_fingerprint()?,
                        instance_sha256: case.instance.fingerprint()?,
                        request_sha256: crate::bounds::sha256(&crate::bounds::encode(
                            &case.eval_case.request,
                            8 * 1024 * 1024,
                        )?),
                    })
                })
                .collect::<Result<_>>()?,
        };
        let plan_bytes = crate::bounds::encode(&plan, 8 * 1024 * 1024)?;
        let plan_sha256 = crate::bounds::sha256(&plan_bytes);
        let existing = self.options.output_dir.exists();
        ensure!(
            !existing || self.options.resume,
            "fresh generation requires a new output directory; existing artifacts were retained"
        );
        let root = if existing {
            StorageRoot::existing(&self.options.output_dir)?
        } else {
            StorageRoot::create(&self.options.output_dir)?
        };
        let _lease = root.lease()?;
        if existing {
            ensure!(
                root.exists("plan.json")?,
                "legacy or incomplete output has no generation plan; use a new output directory"
            );
            ensure!(
                plan.execution_pin.is_some(),
                "resuming generation requires a host execution pin; no agent was started"
            );
            let saved: Plan = serde_json::from_slice(&root.read("plan.json", 8 * 1024 * 1024)?)?;
            ensure!(
                saved == plan,
                "generation inputs changed; old artifacts were retained and no agent was started"
            );
        } else {
            root.publish_json("plan.json", &plan, 8 * 1024 * 1024, false)?;
        }
        let root = Arc::new(root);
        let factory = Arc::new(factory);
        let used = Arc::new(AtomicUsize::new(0));
        let started = Instant::now();
        let mut results = stream::iter(cases.into_iter().map(|case| {
            let root = root.clone();
            let factory = factory.clone();
            let used = used.clone();
            let context = context.clone();
            let plan_sha256 = plan_sha256.clone();
            async move {
                self.run_case(case, root, factory, used, &plan_sha256, context)
                    .await
            }
        }))
        .buffer_unordered(self.options.concurrency)
        .collect::<Vec<_>>()
        .await;
        results.sort_by(|a, b| a.case.instance_id.cmp(&b.case.instance_id));
        // Empty rows are explicit generation failures, preserving the exact selected denominator.
        let predictions = results
            .iter()
            .map(|r| r.prediction.clone())
            .collect::<Vec<_>>();
        let bytes = crate::bounds::encode(&predictions, 256 * 1024 * 1024)?;
        let predictions_sha256 = crate::bounds::sha256(&bytes);
        root.publish_json("predictions.json", &predictions, 256 * 1024 * 1024, true)?;
        let mut summary = SweBenchBatchSummary {
            selected: results.len(),
            predictions: predictions.len(),
            total_tokens: Some(0),
            duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            ..Default::default()
        };
        for result in &results {
            match result.case.status {
                SweBenchBatchCaseStatus::Completed => summary.completed += 1,
                SweBenchBatchCaseStatus::Incomplete => summary.incomplete += 1,
                SweBenchBatchCaseStatus::Failed => summary.failed += 1,
            }
            summary.resumed += usize::from(result.case.resumed);
            summary.empty_patches += usize::from(result.prediction.model_patch.is_empty());
            summary.total_tokens = summary
                .total_tokens
                .zip(result.case.usage.total_tokens)
                .and_then(|(a, b)| a.checked_add(b));
        }
        let report = SweBenchBatchReport {
            schema_version: 4,
            event_schema_version: aicoder_core::events::EVENT_SCHEMA_VERSION,
            run_id: self.options.run_id.clone(),
            dataset: self.options.dataset.clone(),
            model: self.adapter.model().into(),
            model_name_or_path: self.adapter.model_name().into(),
            started_at_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            concurrency: self.options.concurrency,
            resume: self.options.resume,
            parameters: self.options.parameters.clone(),
            predictions_path: self.options.output_dir.join("predictions.json"),
            cases: results.into_iter().map(|r| r.case).collect(),
            summary,
            plan_sha256,
            predictions_sha256,
            execution_pin: self.options.execution_pin.clone(),
        };
        root.publish_json("run.json", &report, 8 * 1024 * 1024, true)?;
        Ok(report)
    }
    async fn run_case<F>(
        &self,
        case: SweBenchCase,
        root: Arc<StorageRoot>,
        factory: Arc<F>,
        used: Arc<AtomicUsize>,
        plan_sha256: &str,
        context: TurnExecutionContext,
    ) -> Done
    where
        F: Fn(&Path) -> Result<TurnExecutor> + Send + Sync + 'static,
    {
        let id = case.instance.instance_id.clone();
        let prefix = format!("cases/{id}");
        let checkpoint = format!("{prefix}/checkpoint.json");
        let mut result = Done {
            case: SweBenchBatchCase {
                instance_id: id.clone(),
                status: SweBenchBatchCaseStatus::Failed,
                resumed: false,
                report_path: None,
                trace_path: None,
                error: None,
                input_sha256: case.input_fingerprint().expect("preflighted input"),
                instance_sha256: case.instance.fingerprint().expect("preflighted instance"),
                artifacts: Vec::new(),
                usage: UsageSummary::default(),
            },
            prediction: SweBenchPrediction {
                instance_id: id.clone(),
                model_name_or_path: self.adapter.model_name().into(),
                model_patch: String::new(),
            },
        };
        let work=async {
            if root.exists(&checkpoint)? {
                let mut receipt:Receipt=serde_json::from_slice(&root.read(&checkpoint,RECORD_LIMIT)?)?;
                ensure!(receipt.schema_version==4 && receipt.plan_sha256==plan_sha256 && receipt.case.input_sha256==result.case.input_sha256 && receipt.case.instance_sha256==result.case.instance_sha256 && receipt.case.instance_id==id && receipt.prediction.instance_id==id && receipt.prediction.model_name_or_path==self.adapter.model_name(),"resume receipt input mismatch");
                verify_case(&root,&receipt.case,&receipt.prediction,self.adapter.model(),Some(&crate::bounds::sha256(&crate::bounds::encode(&case.eval_case.request,8*1024*1024)?)))?;receipt.case.resumed=true;
                return Ok::<_,anyhow::Error>(Some(Done {case:receipt.case,prediction:receipt.prediction}));
            }
            ensure!(!root.exists(&format!("{prefix}/started.json"))?,"an earlier operation may have executed without a complete receipt; no automatic rerun");
            if context.is_cancelled() {anyhow::bail!("generation cancelled before case admission");}
            root.publish_json(&format!("{prefix}/started.json"),&json!({"plan_sha256":plan_sha256,"input_sha256":result.case.input_sha256}),4096,false)?;
            let report=self.eval_runner.run_with_context(&case.eval_case,factory.as_ref(),context).await;
            match report {
                Ok(report)=>{
                    result.case.usage=report.run.usage.clone();
                    result.case.status=match report.run.outcome {crate::EvalRunOutcome::Completed=>SweBenchBatchCaseStatus::Completed,crate::EvalRunOutcome::Incomplete=>SweBenchBatchCaseStatus::Incomplete,crate::EvalRunOutcome::Failed=>SweBenchBatchCaseStatus::Failed};
                    result.case.error=report.run.error.clone();
                    if report.workspace_capture_complete {result.prediction=self.adapter.prediction(&case,&report)?;}else{result.case.status=SweBenchBatchCaseStatus::Failed;result.case.error=Some("workspace capture unavailable; no patch was claimed".into());}
                    let complete=report.trace_capture.complete() && !report.progress.event_delivery.has_failures() && report.trace.validate().is_ok();
                    if !complete {result.case.status=SweBenchBatchCaseStatus::Failed;result.case.error=Some("event observation incomplete".into());}
                    let bytes=crate::bounds::encode(&report,RECORD_LIMIT)?;let _:EvalReport=serde_json::from_slice(&bytes)?;
                    reserve(&used,bytes.len())?;let reference=root.write(&format!("{prefix}/report.json"),&bytes)?;result.case.report_path=Some(self.options.output_dir.join(&reference.path));result.case.artifacts.push(reference);
                    let bytes=crate::bounds::encode(&report.trace.events.iter().map(|e|&e.envelope).collect::<Vec<_>>(),32*1024*1024)?;
                    reserve(&used,bytes.len())?;let reference=root.write(&format!("{prefix}/trace.json"),&bytes)?;result.case.trace_path=Some(self.options.output_dir.join(&reference.path));result.case.artifacts.push(reference);
                }
                Err(error)=>{result.case.error=Some(aicoder_core::redaction::sanitize_text(&format!("Evaluation failed; execution may have occurred: {error:#}"),4096));}
            }
            Ok(None)
        }.await;
        match work {
            Ok(Some(saved)) => return saved,
            Ok(None) => {}
            Err(error) => {
                if result.case.report_path.is_none() {
                    result.prediction.model_patch.clear();
                }
                result.case.status = SweBenchBatchCaseStatus::Failed;
                result.case.error = Some(aicoder_core::redaction::sanitize_text(
                    &format!("No operation was retried: {error:#}"),
                    4096,
                ));
                return result;
            }
        }
        let receipt = Receipt {
            schema_version: 4,
            plan_sha256: plan_sha256.into(),
            case: result.case.clone(),
            prediction: result.prediction.clone(),
        };
        if let Err(error) = root.publish_json(&checkpoint, &receipt, RECORD_LIMIT, false) {
            result.case.status = SweBenchBatchCaseStatus::Failed;
            result.case.error = Some(format!(
                "Receipt publication failed after execution; no retry: {error}"
            ));
        }
        result
    }
}
fn reserve(used: &AtomicUsize, bytes: usize) -> Result<()> {
    used.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
        n.checked_add(bytes).filter(|v| *v <= DISK_LIMIT)
    })
    .map_err(|_| anyhow::anyhow!("generation archive disk admission exhausted"))?;
    Ok(())
}
fn read_ref(root: &StorageRoot, reference: &OutputRef) -> Result<Vec<u8>> {
    ensure!(
        reference.bytes <= RECORD_LIMIT,
        "generation artifact byte budget exceeded"
    );
    let bytes = root.read(&reference.path, reference.bytes)?;
    ensure!(
        bytes.len() == reference.bytes && crate::bounds::sha256(&bytes) == reference.sha256,
        "generation artifact mismatch"
    );
    Ok(bytes)
}
fn verify_case(
    root: &StorageRoot,
    case: &SweBenchBatchCase,
    prediction: &SweBenchPrediction,
    model: &str,
    request_sha: Option<&str>,
) -> Result<()> {
    ensure!(
        case.artifacts.len() <= 2 && prediction.instance_id == case.instance_id,
        "invalid generation artifact/case identity"
    );
    let mut names = BTreeSet::new();
    for artifact in &case.artifacts {
        ensure!(
            [
                format!("cases/{}/report.json", case.instance_id),
                format!("cases/{}/trace.json", case.instance_id)
            ]
            .contains(&artifact.path)
                && names.insert(&artifact.path),
            "foreign or duplicate generation artifact"
        );
        read_ref(root, artifact)?;
    }
    if case.report_path.is_some() {
        let reference = case
            .artifacts
            .iter()
            .find(|a| a.path.ends_with("/report.json"))
            .ok_or_else(|| anyhow::anyhow!("generation report missing"))?;
        let report: EvalReport = serde_json::from_slice(&read_ref(root, reference)?)?;
        ensure!(
            report.case.id == case.instance_id
                && report.case.model == model
                && report.run.usage == case.usage,
            "generation report metrics mismatch"
        );
        if let Some(hash) = request_sha {
            ensure!(
                report
                    .provenance
                    .as_ref()
                    .is_some_and(|p| p.request_sha256 == hash),
                "generation prompt differs from plan"
            );
        }
        let expected = if report.workspace_capture_complete {
            report.workspace_patch.as_deref().unwrap_or("")
        } else {
            ""
        };
        ensure!(
            prediction.model_patch == expected,
            "prediction patch differs from the captured generation"
        );
        let trace = case
            .artifacts
            .iter()
            .find(|a| a.path.ends_with("/trace.json"));
        if let Some(trace) = trace {
            let data = crate::AgentTrace::decode_json(
                &read_ref(root, trace)?,
                crate::TraceLimits::default(),
            )?;
            ensure!(
                data.summary() == report.trajectory
                    && data.events.len() == report.trace_capture.retained_events,
                "generation trace differs from report"
            );
            let complete = report.workspace_capture_complete
                && report.trace_capture.complete()
                && !report.progress.event_delivery.has_failures()
                && data.validate().is_ok();
            let expected = if !complete {
                SweBenchBatchCaseStatus::Failed
            } else {
                match report.run.outcome {
                    crate::EvalRunOutcome::Completed => SweBenchBatchCaseStatus::Completed,
                    crate::EvalRunOutcome::Incomplete => SweBenchBatchCaseStatus::Incomplete,
                    crate::EvalRunOutcome::Failed => SweBenchBatchCaseStatus::Failed,
                }
            };
            ensure!(
                case.status == expected,
                "generation status differs from actual completion/evidence"
            );
            if complete {
                let terminal = data
                    .events
                    .last()
                    .and_then(|e| e.envelope.termination.as_ref())
                    .ok_or_else(|| anyhow::anyhow!("generation terminal missing"))?;
                ensure!(
                    terminal.usage == report.progress.usage
                        && terminal.tool_calls == report.progress.tool_calls
                        && terminal.stop == report.run.stop,
                    "generation terminal metrics mismatch"
                );
            }
        } else {
            ensure!(
                case.status == SweBenchBatchCaseStatus::Failed,
                "successful generation has no trace"
            );
        }
    } else {
        ensure!(
            case.status == SweBenchBatchCaseStatus::Failed
                && case.artifacts.is_empty()
                && prediction.model_patch.is_empty(),
            "unobserved generation cannot claim a patch"
        );
    }
    Ok(())
}
impl SweBenchBatchReport {
    /// Read only owned data, cross-check every selected row and preserve failed placeholders.
    pub fn load_verified(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        ensure!(
            path.file_name().is_some_and(|n| n == "run.json"),
            "generation manifest must be run.json"
        );
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let root = StorageRoot::existing(parent)?;
        let _lease = root.lease()?;
        let report: Self = serde_json::from_slice(&root.read("run.json", 8 * 1024 * 1024)?)?;
        ensure!(
            report.schema_version == 4
                && report.event_schema_version == aicoder_core::events::EVENT_SCHEMA_VERSION,
            "unsupported generation manifest"
        );
        let bytes = root.read("plan.json", 8 * 1024 * 1024)?;
        ensure!(
            crate::bounds::sha256(&bytes) == report.plan_sha256,
            "generation plan fingerprint mismatch"
        );
        let plan: Plan = serde_json::from_slice(&bytes)?;
        ensure!(
            plan.schema_version == 4
                && plan.run_id == report.run_id
                && plan.dataset == report.dataset
                && plan.model == report.model
                && plan.model_name_or_path == report.model_name_or_path
                && plan.execution_pin == report.execution_pin
                && plan.concurrency == report.concurrency
                && plan.parameters == report.parameters,
            "generation manifest differs from plan"
        );
        let bytes = root.read("predictions.json", 256 * 1024 * 1024)?;
        ensure!(
            crate::bounds::sha256(&bytes) == report.predictions_sha256,
            "prediction table fingerprint mismatch"
        );
        let predictions: Vec<SweBenchPrediction> = serde_json::from_slice(&bytes)?;
        ensure!(
            plan.cases.len() == report.cases.len()
                && predictions.len() == report.cases.len()
                && !report.cases.is_empty()
                && report.cases.len() <= 5000,
            "generation selection/denominator mismatch"
        );
        let mut summary = SweBenchBatchSummary {
            selected: report.cases.len(),
            predictions: predictions.len(),
            total_tokens: Some(0),
            duration_ms: report.summary.duration_ms,
            ..Default::default()
        };
        let mut ids = BTreeSet::new();
        for ((case, input), prediction) in report.cases.iter().zip(&plan.cases).zip(&predictions) {
            ensure!(
                crate::swebench::valid_instance_id(&case.instance_id)
                    && ids.insert(&case.instance_id)
                    && case.instance_id == input.id
                    && case.input_sha256 == input.input_sha256
                    && case.instance_sha256 == input.instance_sha256
                    && prediction.instance_id == case.instance_id
                    && prediction.model_name_or_path == report.model_name_or_path,
                "foreign/duplicate generation input"
            );
            verify_case(
                &root,
                case,
                prediction,
                &report.model,
                Some(&input.request_sha256),
            )?;
            match case.status {
                SweBenchBatchCaseStatus::Completed => summary.completed += 1,
                SweBenchBatchCaseStatus::Incomplete => summary.incomplete += 1,
                SweBenchBatchCaseStatus::Failed => summary.failed += 1,
            }
            summary.resumed += usize::from(case.resumed);
            summary.empty_patches += usize::from(prediction.model_patch.is_empty());
            summary.total_tokens = summary
                .total_tokens
                .zip(case.usage.total_tokens)
                .and_then(|(a, b)| a.checked_add(b));
        }
        ensure!(
            summary == report.summary,
            "generation totals differ from selected rows"
        );
        Ok(report)
    }
}
