//! Versioned coding benchmark archives over EvalRunner/core, independent of frontend text.
mod archive;
mod measure;
mod suite;
use aicoder_core::{TurnExecutionContext, TurnExecutor, model::ModelRequest};
use anyhow::{Result, ensure};
pub use archive::{BenchArtifact, BenchComparison};
pub use measure::{MemoryObservation, SourceStamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
pub use suite::{CodingCase, CodingSuite, Grading, ScriptVariant, ScriptedCodingProvider};
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchMode {
    ScriptedContract,
    LiveProvider,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchCaseStatus {
    Completed,
    Incomplete,
    Failed,
    InfrastructureError,
    ArtifactError,
    NotRun,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CostEstimate {
    pub currency: String,
    pub amount: f64,
    pub basis: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchCaseResult {
    pub id: String,
    pub repetition: usize,
    pub status: BenchCaseStatus,
    pub verdict: crate::EvalVerdict,
    pub outcome: Option<crate::EvalRunOutcome>,
    pub input_sha256: Option<String>,
    pub duration_ms: u64,
    pub agent_duration_ms: Option<u64>,
    pub usage: crate::UsageSummary,
    pub tool_calls: Option<usize>,
    pub tool_errors: Option<usize>,
    pub stop: Option<aicoder_core::RunStop>,
    pub trace_valid: bool,
    pub memory: MemoryObservation,
    pub cost: Option<CostEstimate>,
    pub artifacts: Vec<BenchArtifact>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchSummary {
    pub selected: usize,
    pub passed: usize,
    pub failed: usize,
    pub inconclusive: usize,
    pub evaluator_errors: usize,
    pub completed: usize,
    pub incomplete: usize,
    pub run_failures: usize,
    pub infrastructure_errors: usize,
    pub not_run: usize,
    pub pass_rate: f64,
    pub total_tokens: Option<u64>,
    pub total_tool_calls: Option<usize>,
    pub total_tool_errors: Option<usize>,
    pub median_case_ms: Option<u64>,
    pub p95_case_ms: Option<u64>,
    pub sampled_process_rss_kib: Option<u64>,
    pub cost: Option<CostEstimate>,
}
impl BenchSummary {
    fn from_cases(cases: &[BenchCaseResult]) -> Self {
        let mut summary = Self {
            selected: cases.len(),
            total_tokens: Some(0),
            total_tool_calls: Some(0),
            total_tool_errors: Some(0),
            ..Default::default()
        };
        let mut costs = Vec::new();
        let mut durations = Vec::new();
        for case in cases {
            match case.verdict {
                crate::EvalVerdict::Passed => summary.passed += 1,
                crate::EvalVerdict::Failed => summary.failed += 1,
                crate::EvalVerdict::Inconclusive => summary.inconclusive += 1,
                crate::EvalVerdict::EvaluatorError => summary.evaluator_errors += 1,
            }
            match case.status {
                BenchCaseStatus::Completed => summary.completed += 1,
                BenchCaseStatus::Incomplete => summary.incomplete += 1,
                BenchCaseStatus::Failed => summary.run_failures += 1,
                BenchCaseStatus::InfrastructureError | BenchCaseStatus::ArtifactError => {
                    summary.infrastructure_errors += 1
                }
                BenchCaseStatus::NotRun => summary.not_run += 1,
            }
            summary.total_tokens = summary
                .total_tokens
                .zip(case.usage.total_tokens)
                .and_then(|(a, b)| a.checked_add(b));
            summary.total_tool_calls = summary
                .total_tool_calls
                .zip(case.tool_calls)
                .and_then(|(a, b)| a.checked_add(b));
            summary.total_tool_errors = summary
                .total_tool_errors
                .zip(case.tool_errors)
                .and_then(|(a, b)| a.checked_add(b));
            if let Some(kib) = case.memory.sampled_peak_rss_kib {
                summary.sampled_process_rss_kib =
                    Some(summary.sampled_process_rss_kib.unwrap_or(0).max(kib));
            }
            durations.push(case.duration_ms);
            costs.push(case.cost.as_ref());
        }
        if !cases.is_empty() {
            summary.pass_rate = summary.passed as f64 / cases.len() as f64;
            durations.sort_unstable();
            summary.median_case_ms = Some(durations[(durations.len() - 1) / 2]);
            summary.p95_case_ms = Some(durations[(durations.len() * 95).div_ceil(100) - 1]);
            if let Some(first) = costs[0]
                && costs
                    .iter()
                    .all(|c| c.is_some_and(|c| c.currency == first.currency))
            {
                let amount = costs.iter().flatten().map(|c| c.amount).sum::<f64>();
                if amount.is_finite() {
                    summary.cost = Some(CostEstimate {
                        currency: first.currency.clone(),
                        amount,
                        basis: first.basis.clone(),
                    });
                }
            }
        }
        summary
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchReport {
    pub schema_version: u32,
    pub event_schema_version: u32,
    pub suite_id: String,
    pub suite_sha256: String,
    pub source: SourceStamp,
    pub mode: BenchMode,
    pub repetitions: usize,
    pub selected_ids: Vec<String>,
    pub host_parameters: Value,
    pub cancelled: bool,
    pub cases: Vec<BenchCaseResult>,
    pub summary: BenchSummary,
}
impl BenchReport {
    fn validate_structure(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1
                && self.event_schema_version == aicoder_core::events::EVENT_SCHEMA_VERSION,
            "unsupported benchmark archive version"
        );
        ensure!(
            suite::valid_id(&self.suite_id)
                && (1..=8).contains(&self.repetitions)
                && !self.selected_ids.is_empty()
                && self.selected_ids.len() <= 32,
            "invalid benchmark plan"
        );
        ensure!(
            self.suite_sha256.len() == 64
                && self.suite_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid suite fingerprint"
        );
        let mut ids = std::collections::BTreeSet::new();
        ensure!(
            self.selected_ids
                .iter()
                .all(|id| suite::valid_id(id) && ids.insert(id)),
            "invalid or duplicate benchmark selection"
        );
        ensure!(
            self.cases.len() == self.selected_ids.len() * self.repetitions,
            "selected cases missing from denominator"
        );
        self.source.validate()?;
        crate::bounds::encoded_size(&self.host_parameters, 1024 * 1024)?;
        let mut artifacts = std::collections::BTreeSet::new();
        for (index, case) in self.cases.iter().enumerate() {
            ensure!(
                case.id == self.selected_ids[index / self.repetitions]
                    && case.repetition == index % self.repetitions,
                "case ordering/selection mismatch"
            );
            match case.status {
                BenchCaseStatus::InfrastructureError | BenchCaseStatus::ArtifactError => ensure!(
                    case.verdict == crate::EvalVerdict::EvaluatorError,
                    "infrastructure/artifact errors cannot masquerade as graded outcomes"
                ),
                BenchCaseStatus::NotRun => ensure!(
                    case.verdict == crate::EvalVerdict::Inconclusive
                        && case.outcome.is_none()
                        && case.tool_calls.is_none()
                        && case.tool_errors.is_none()
                        && case.usage == crate::UsageSummary::default()
                        && case.cost.is_none()
                        && case.artifacts.is_empty()
                        && !case.trace_valid,
                    "unstarted case cannot claim execution or measurement"
                ),
                _ => {}
            }
            ensure!(case.artifacts.len() <= 4, "artifact count budget exceeded");
            for artifact in &case.artifacts {
                suite::relative(&artifact.path)?;
                ensure!(
                    artifact.sha256.len() == 64
                        && artifact.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid artifact digest"
                );
                let prefix = format!("cases/{}-{}/", case.id, case.repetition);
                ensure!(
                    artifact.path.starts_with(&prefix) && artifacts.insert(&artifact.path),
                    "foreign or duplicate benchmark artifact"
                );
            }
            ensure!(
                case.verdict != crate::EvalVerdict::Passed
                    || (case.status == BenchCaseStatus::Completed
                        && case.trace_valid
                        && case.input_sha256.is_some()),
                "invalid successful benchmark claim"
            );
        }
        ensure!(
            self.summary == BenchSummary::from_cases(&self.cases),
            "benchmark summary differs from selected cases"
        );
        Ok(())
    }
}
#[derive(Clone)]
pub struct BenchOptions {
    pub output: PathBuf,
    pub source: SourceStamp,
    pub mode: BenchMode,
    pub repetitions: usize,
    pub selected_ids: Vec<String>,
    pub host_parameters: Value,
    pub excluded_environment: Vec<String>,
}
impl BenchOptions {
    pub fn new(output: impl Into<PathBuf>, source: SourceStamp, mode: BenchMode) -> Self {
        Self {
            output: output.into(),
            source,
            mode,
            repetitions: 1,
            selected_ids: Vec::new(),
            host_parameters: Value::Null,
            excluded_environment: Vec::new(),
        }
    }
}
/// Sequential, bounded, no implicit resume/re-execution. Each case goes through EvalRunner.
pub struct BenchRunner {
    suite: CodingSuite,
    options: BenchOptions,
}
impl BenchRunner {
    pub fn new(suite: CodingSuite, options: BenchOptions) -> Result<Self> {
        suite.validate()?;
        options.source.validate()?;
        ensure!(
            (1..=8).contains(&options.repetitions),
            "benchmark repetition budget exceeded"
        );
        crate::bounds::encoded_size(&options.host_parameters, 1024 * 1024)?;
        let _: Value = serde_json::from_slice(&crate::bounds::encode(
            &json!({"host_parameters":options.host_parameters}),
            1024 * 1024 + 32,
        )?)?;
        Ok(Self { suite, options })
    }
    pub async fn run<F, Q>(
        &self,
        requests: Q,
        factory: F,
        context: TurnExecutionContext,
    ) -> Result<BenchReport>
    where
        F: Fn(&CodingCase, &Path) -> Result<TurnExecutor>,
        Q: Fn(&CodingCase) -> ModelRequest,
    {
        let selected = if self.options.selected_ids.is_empty() {
            self.suite
                .cases
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>()
        } else {
            self.options.selected_ids.clone()
        };
        let mut seen = std::collections::BTreeSet::new();
        for id in &selected {
            ensure!(
                seen.insert(id) && self.suite.cases.iter().any(|c| &c.id == id),
                "unknown or duplicate selected benchmark case"
            );
        }
        let storage = archive::ArchiveRoot::create(&self.options.output)?;
        let mut report = BenchReport {
            schema_version: 1,
            event_schema_version: aicoder_core::events::EVENT_SCHEMA_VERSION,
            suite_id: self.suite.id.clone(),
            suite_sha256: self.suite.fingerprint()?,
            source: self.options.source.clone(),
            mode: self.options.mode,
            repetitions: self.options.repetitions,
            selected_ids: selected.clone(),
            host_parameters: self.options.host_parameters.clone(),
            cancelled: false,
            cases: Vec::new(),
            summary: BenchSummary::default(),
        };
        let mut stored_bytes = 0usize;
        for id in selected {
            let case = self
                .suite
                .cases
                .iter()
                .find(|c| c.id == id)
                .expect("selected case");
            for repetition in 0..self.options.repetitions {
                let started = Instant::now();
                let mut entry = BenchCaseResult {
                    id: id.clone(),
                    repetition,
                    status: BenchCaseStatus::NotRun,
                    verdict: crate::EvalVerdict::Inconclusive,
                    outcome: None,
                    input_sha256: None,
                    duration_ms: 0,
                    agent_duration_ms: None,
                    usage: crate::UsageSummary::default(),
                    tool_calls: None,
                    tool_errors: None,
                    stop: None,
                    trace_valid: false,
                    memory: MemoryObservation::default(),
                    cost: None,
                    artifacts: Vec::new(),
                    error: None,
                };
                if context.is_cancelled() {
                    entry.error = Some("Not started: benchmark cancellation".into());
                    report.cases.push(entry);
                    continue;
                }
                let outcome = async {
                    let fixture = tempfile::tempdir()?;
                    case.materialize(fixture.path())?;
                    let eval_case = crate::EvalCase::new(
                        &case.id,
                        requests(case),
                        crate::WorkspaceFixture::CopyFrom(fixture.path().into()),
                    );
                    let mut changes =
                        crate::WorkspaceDiffEvaluator::new().forbid_unlisted_changes(true);
                    for path in &case.allowed_changes {
                        changes = changes.allow_prefix(path);
                    }
                    let runner = crate::EvalRunner::new().evaluator(changes).evaluator(
                        suite::CodingGrader {
                            case: case.clone(),
                            excluded: self.options.excluded_environment.clone(),
                        },
                    );
                    let meter = measure::MemoryMeter::start()?;
                    let result = runner
                        .run_with_context(
                            &eval_case,
                            &|workspace| factory(case, workspace),
                            context.clone(),
                        )
                        .await;
                    entry.memory = meter.finish();
                    result
                }
                .await;
                match outcome {
                    Err(error) => {
                        entry.status = BenchCaseStatus::InfrastructureError;
                        entry.verdict = crate::EvalVerdict::EvaluatorError;
                        entry.error = Some(aicoder_core::redaction::sanitize_text(
                            &format!("Evaluation failed; execution may have occurred: {error:#}"),
                            4096,
                        ));
                    }
                    Ok(run) => {
                        entry.status = match run.run.outcome {
                            crate::EvalRunOutcome::Completed => BenchCaseStatus::Completed,
                            crate::EvalRunOutcome::Incomplete => BenchCaseStatus::Incomplete,
                            crate::EvalRunOutcome::Failed => BenchCaseStatus::Failed,
                        };
                        entry.outcome = Some(run.run.outcome);
                        entry.verdict = run.verdict;
                        entry.agent_duration_ms = Some(run.run.duration_ms);
                        entry.usage = run.run.usage.clone();
                        entry.tool_calls = Some(run.progress.tool_calls);
                        entry.tool_errors = Some(run.trajectory.failed_tool_calls);
                        entry.stop = run.run.stop.clone();
                        entry.error = run.run.error.clone();
                        let identity = run
                            .provenance
                            .as_ref()
                            .filter(|p| p.model.is_some())
                            .map(input_fingerprint)
                            .transpose();
                        match identity {
                            Ok(value) => entry.input_sha256 = value,
                            Err(error) => {
                                entry.status = BenchCaseStatus::ArtifactError;
                                entry.verdict = crate::EvalVerdict::EvaluatorError;
                                entry.error =
                                    Some(format!("Input identity could not be retained: {error}"));
                            }
                        }
                        entry.trace_valid = run.trace_capture.complete()
                            && !run.progress.event_delivery.has_failures()
                            && run.trace.validate().is_ok();
                        if entry.status == BenchCaseStatus::ArtifactError {
                            entry.trace_valid = false;
                        }
                        entry.tool_errors = entry
                            .trace_valid
                            .then_some(run.trajectory.failed_tool_calls);
                        entry.cost = cost_estimate(&run, self.options.mode);
                        let prefix = format!("cases/{}-{}", id, repetition);
                        let artifacts = (|| -> Result<()> {
                            let mut save = |name: &str, bytes: Vec<u8>| -> Result<()> {
                                ensure!(
                                    bytes.len() <= 64 * 1024 * 1024
                                        && bytes.len()
                                            <= (512 * 1024 * 1024_usize)
                                                .saturating_sub(stored_bytes),
                                    "benchmark disk admission budget exhausted"
                                );
                                let reference =
                                    storage.write(&format!("{prefix}/{name}"), &bytes)?;
                                stored_bytes += bytes.len();
                                entry.artifacts.push(reference);
                                Ok(())
                            };
                            let bytes = crate::bounds::encode(&run, 16 * 1024 * 1024)?;
                            let _: crate::EvalReport = serde_json::from_slice(&bytes)?;
                            save("report.json", bytes)?;
                            save(
                                "trace.json",
                                crate::bounds::encode(
                                    &run.trace
                                        .events
                                        .iter()
                                        .map(|e| &e.envelope)
                                        .collect::<Vec<_>>(),
                                    32 * 1024 * 1024,
                                )?,
                            )?;
                            save(
                                "workspace-diff.json",
                                crate::bounds::encode(&run.workspace_diff, 1024 * 1024)?,
                            )?;
                            if let Some(patch) = &run.workspace_patch {
                                save("workspace.patch", patch.as_bytes().to_vec())?;
                            }
                            Ok(())
                        })();
                        if let Err(error) = artifacts {
                            entry.status = BenchCaseStatus::ArtifactError;
                            entry.verdict = crate::EvalVerdict::EvaluatorError;
                            entry.trace_valid = false;
                            entry.error = Some(format!(
                                "Operations were not retried; artifact publication failed: {error}"
                            ));
                        }
                    }
                }
                entry.duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                report.cases.push(entry);
            }
        }
        report.cancelled = context.is_cancelled();
        report.summary = BenchSummary::from_cases(&report.cases);
        report.validate_structure()?;
        storage.finish(&report)?;
        Ok(report)
    }
}
fn input_fingerprint(provenance: &crate::EvalProvenance) -> Result<String> {
    fn normalize(value: &mut Value, workspace: &str) {
        match value {
            Value::String(text) if text.starts_with(workspace) => {
                *text = format!("<workspace>{}", &text[workspace.len()..]);
            }
            Value::Array(values) => {
                for value in values {
                    normalize(value, workspace)
                }
            }
            Value::Object(values) => {
                for value in values.values_mut() {
                    normalize(value, workspace)
                }
            }
            _ => {}
        }
    }
    let context = provenance
        .journal
        .iter()
        .filter(|v| v["kind"] == "input_context")
        .collect::<Vec<_>>();
    let mut value = json!({"request":provenance.request_sha256,"model_options":provenance.model_options,"requested_stream":provenance.requested_stream,"workspace":provenance.workspace_sha256,"model":provenance.model,"execution":provenance.execution,"dispatcher":provenance.dispatcher,"tools":provenance.tools,"tool_capabilities":provenance.tool_capabilities,"extensions":provenance.extensions,"context_policy":provenance.context_policy,"input_context":context});
    normalize(&mut value, &provenance.workspace.to_string_lossy());
    Ok(crate::bounds::sha256(&crate::bounds::encode(
        &value,
        4 * 1024 * 1024,
    )?))
}
fn cost_estimate(run: &crate::EvalReport, mode: BenchMode) -> Option<CostEstimate> {
    if mode != BenchMode::LiveProvider
        || !run.progress.reported_usage
        || run.progress.usage.cached_input_tokens.unwrap_or(0) > 0
        || run.progress.usage.cache_write_tokens.unwrap_or(0) > 0
        || run
            .progress
            .model_context
            .as_ref()
            .is_some_and(|m| m.summary_attempts > 0)
    {
        return None;
    }
    let price = run.provenance.as_ref()?.model.as_ref()?.pricing.as_ref()?;
    let amount = run.progress.usage.input_tokens? as f64 * price.input_per_million / 1_000_000.0
        + run.progress.usage.output_tokens? as f64 * price.output_per_million / 1_000_000.0;
    (amount.is_finite() && amount >= 0.0).then(|| CostEstimate {
        currency: price.currency.clone(),
        amount,
        basis: "estimate_from_reported_tokens_and_declared_base_rates_not_billed_cost".into(),
    })
}
