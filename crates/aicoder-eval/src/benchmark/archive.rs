use super::*;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Component, PathBuf},
};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BenchArtifact {
    pub path: String,
    pub bytes: usize,
    pub sha256: String,
}
/// Linux file-descriptor anchors keep even renamed directories inside the selected archive.
pub(super) struct ArchiveRoot {
    directory: File,
}
fn descriptor_path(file: &File) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        PathBuf::new()
    }
}
fn open(path: &Path, directory: bool, write: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(!write).write(write).create_new(write);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NOFOLLOW | if directory { libc::O_DIRECTORY } else { 0 })
            .mode(0o600);
    }
    let file = options.open(path)?;
    ensure!(
        if directory {
            file.metadata()?.is_dir()
        } else {
            file.metadata()?.is_file()
        },
        "unexpected benchmark storage file type"
    );
    Ok(file)
}
impl ArchiveRoot {
    pub fn existing(root: &Path) -> Result<Self> {
        ensure!(
            cfg!(target_os = "linux"),
            "pinned benchmark archives currently require Linux /proc/self/fd"
        );
        Ok(Self {
            directory: open(root, true, false)?,
        })
    }
    pub fn create(root: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(root)?;
        Self::existing(root)
    }
    fn parent(&self, path: &str, create: bool) -> Result<(File, String)> {
        suite::relative(path)?;
        let mut parts = Path::new(path)
            .components()
            .map(|c| match c {
                Component::Normal(p) => p.to_str().expect("validated UTF-8"),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        ensure!(parts.len() <= 16, "benchmark artifact depth budget");
        let leaf = parts.pop().expect("nonempty relative path").to_string();
        let mut directory = self.directory.try_clone()?;
        for part in parts {
            let next = descriptor_path(&directory).join(part);
            if create {
                match std::fs::create_dir(&next) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
            }
            directory = open(&next, true, false)?;
        }
        Ok((directory, leaf))
    }
    pub fn read(&self, path: &str, limit: usize) -> Result<Vec<u8>> {
        let (parent, name) = self.parent(path, false)?;
        let file = open(&descriptor_path(&parent).join(name), false, false)?;
        ensure!(
            file.metadata()?.len() <= limit as u64,
            "benchmark artifact byte limit"
        );
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= limit,
            "benchmark artifact grew beyond budget"
        );
        Ok(bytes)
    }
    pub fn write(&self, path: &str, bytes: &[u8]) -> Result<BenchArtifact> {
        let (parent, name) = self.parent(path, true)?;
        let mut file = open(&descriptor_path(&parent).join(name), false, true)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(BenchArtifact {
            path: path.into(),
            bytes: bytes.len(),
            sha256: crate::bounds::sha256(bytes),
        })
    }
    pub fn finish(&self, report: &BenchReport) -> Result<()> {
        let bytes = crate::bounds::encode(report, 4 * 1024 * 1024)?;
        self.write("manifest.pending", &bytes)?;
        let root = descriptor_path(&self.directory);
        // Link is atomic and fails if another manifest exists; no overwrite or automatic retry.
        std::fs::hard_link(root.join("manifest.pending"), root.join("manifest.json"))?;
        self.directory.sync_all()?;
        std::fs::remove_file(root.join("manifest.pending"))?;
        self.directory.sync_all()?;
        Ok(())
    }
}
impl BenchReport {
    pub fn replay_case(
        root: impl AsRef<Path>,
        id: &str,
        repetition: usize,
    ) -> Result<crate::AgentTrace> {
        let report = Self::verify(root.as_ref())?;
        let case = report
            .cases
            .iter()
            .find(|c| c.id == id && c.repetition == repetition)
            .ok_or_else(|| anyhow::anyhow!("case not found"))?;
        ensure!(
            case.trace_valid,
            "incomplete trace cannot be replayed as complete"
        );
        let reference = case
            .artifacts
            .iter()
            .find(|a| a.path.ends_with("/trace.json"))
            .ok_or_else(|| anyhow::anyhow!("trace missing"))?;
        let storage = ArchiveRoot::existing(root.as_ref())?;
        let bytes = read_artifact(&storage, reference)?;
        let trace = crate::AgentTrace::decode_json(&bytes, crate::TraceLimits::default())?;
        trace.validate()?;
        Ok(trace)
    }
    /// Validate the archive, recompute summaries and replay typed events. Never runs a tool/model.
    pub fn verify(root: impl AsRef<Path>) -> Result<Self> {
        let storage = ArchiveRoot::existing(root.as_ref())?;
        let mut report: Self =
            serde_json::from_slice(&storage.read("manifest.json", 4 * 1024 * 1024)?)?;
        report.validate_structure()?;
        let mut total = 0usize;
        for case in &report.cases {
            for artifact in &case.artifacts {
                total = total
                    .checked_add(artifact.bytes)
                    .ok_or_else(|| anyhow::anyhow!("archive size overflow"))?;
                ensure!(
                    total <= 512 * 1024 * 1024 && artifact.bytes <= 64 * 1024 * 1024,
                    "archive byte budget exceeded"
                );
                let bytes = storage.read(&artifact.path, artifact.bytes)?;
                ensure!(
                    bytes.len() == artifact.bytes
                        && crate::bounds::sha256(&bytes) == artifact.sha256,
                    "benchmark artifact fingerprint mismatch"
                );
            }
            if let Some(reference) = case
                .artifacts
                .iter()
                .find(|a| a.path.ends_with("/report.json"))
            {
                let run: crate::EvalReport =
                    serde_json::from_slice(&read_artifact(&storage, reference)?)?;
                let expected_outcome = match &run.run.stop {
                    None => crate::EvalRunOutcome::Completed,
                    Some(aicoder_core::RunStop::Budget(_) | aicoder_core::RunStop::Model(_)) => {
                        crate::EvalRunOutcome::Incomplete
                    }
                    Some(_) => crate::EvalRunOutcome::Failed,
                };
                ensure!(
                    run.run.outcome == expected_outcome,
                    "outcome disagrees with stop reason"
                );
                let expected_status = match run.run.outcome {
                    crate::EvalRunOutcome::Completed => BenchCaseStatus::Completed,
                    crate::EvalRunOutcome::Incomplete => BenchCaseStatus::Incomplete,
                    crate::EvalRunOutcome::Failed => BenchCaseStatus::Failed,
                };
                ensure!(
                    case.status == expected_status || case.status == BenchCaseStatus::ArtifactError,
                    "case status disagrees with core outcome"
                );
                let override_error = case.status == BenchCaseStatus::ArtifactError
                    && case.verdict == crate::EvalVerdict::EvaluatorError;
                ensure!(
                    (case.verdict == run.verdict || override_error)
                        && case.outcome == Some(run.run.outcome)
                        && case.tool_calls == Some(run.progress.tool_calls)
                        && case.usage == run.run.usage
                        && case.stop == run.run.stop
                        && case.agent_duration_ms == Some(run.run.duration_ms)
                        && case.duration_ms >= run.run.duration_ms
                        && case.cost == cost_estimate(&run, report.mode),
                    "manifest metrics disagree with case report"
                );
                ensure!(
                    case.input_sha256
                        == run
                            .provenance
                            .as_ref()
                            .filter(|p| p.model.is_some())
                            .map(input_fingerprint)
                            .transpose()?,
                    "case input identity mismatch"
                );
                if override_error {
                    ensure!(
                        !case.trace_valid,
                        "artifact failure cannot claim complete observation"
                    );
                    continue;
                }
                let trace_ref = case
                    .artifacts
                    .iter()
                    .find(|a| a.path.ends_with("/trace.json"))
                    .ok_or_else(|| anyhow::anyhow!("missing trace artifact"))?;
                let bytes = read_artifact(&storage, trace_ref)?;
                let trace = crate::AgentTrace::decode_json(&bytes, crate::TraceLimits::default())?;
                ensure!(
                    trace.events.len() == run.trace_capture.retained_events
                        && (run.trace_capture.dropped_events == 0)
                            == run.trace_capture.first_missing_sequence.is_none(),
                    "trace retention metadata differs"
                );
                let retained = trace.events.iter().try_fold(0usize, |sum, e| {
                    crate::bounds::encoded_size(&e.envelope, 32 * 1024 * 1024).map(|n| sum + n)
                })?;
                ensure!(
                    retained == run.trace_capture.retained_bytes,
                    "trace byte count mismatch"
                );
                let valid = run.trace_capture.complete()
                    && !run.progress.event_delivery.has_failures()
                    && trace.validate().is_ok();
                ensure!(
                    case.trace_valid == valid
                        && trace.summary() == run.trajectory
                        && case.tool_errors == valid.then_some(run.trajectory.failed_tool_calls),
                    "trace completeness/summary mismatch"
                );
                if valid {
                    let terminal = trace
                        .events
                        .last()
                        .and_then(|e| e.envelope.termination.as_ref())
                        .ok_or_else(|| anyhow::anyhow!("trace terminal summary missing"))?;
                    ensure!(
                        terminal.usage == run.progress.usage
                            && terminal.tool_calls == run.progress.tool_calls
                            && terminal.rounds == run.progress.rounds
                            && terminal.stop == run.run.stop
                            && terminal.output_bytes == run.progress.output_bytes,
                        "run report differs from terminal event"
                    );
                }
                let diff_ref = case
                    .artifacts
                    .iter()
                    .find(|a| a.path.ends_with("/workspace-diff.json"))
                    .ok_or_else(|| anyhow::anyhow!("workspace diff artifact missing"))?;
                let diff: crate::WorkspaceDiff =
                    serde_json::from_slice(&read_artifact(&storage, diff_ref)?)?;
                ensure!(
                    diff == run.workspace_diff,
                    "workspace diff artifact mismatch"
                );
                if let Some(patch) = &run.workspace_patch {
                    let patch_ref = case
                        .artifacts
                        .iter()
                        .find(|a| a.path.ends_with("/workspace.patch"))
                        .ok_or_else(|| anyhow::anyhow!("patch artifact missing"))?;
                    ensure!(
                        read_artifact(&storage, patch_ref)? == patch.as_bytes(),
                        "patch artifact differs from report"
                    );
                }
                if case.verdict == crate::EvalVerdict::Passed {
                    ensure!(
                        valid
                            && run.run.outcome == crate::EvalRunOutcome::Completed
                            && run.workspace_capture_complete
                            && !run.evaluations.is_empty()
                            && run
                                .evaluations
                                .iter()
                                .all(|e| e.verdict == crate::EvalVerdict::Passed),
                        "success requires complete observation and passing graders"
                    );
                }
            } else {
                ensure!(
                    matches!(
                        case.status,
                        BenchCaseStatus::InfrastructureError
                            | BenchCaseStatus::NotRun
                            | BenchCaseStatus::ArtifactError
                    ) && case.verdict != crate::EvalVerdict::Passed,
                    "missing executed case report"
                );
            }
        }
        let saved = report.summary.clone();
        report.summary = BenchSummary::from_cases(&report.cases);
        ensure!(
            report.summary == saved,
            "benchmark summary does not match selected cases"
        );
        Ok(report)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchComparison {
    pub selected: usize,
    pub left_passed: usize,
    pub right_passed: usize,
    pub both_passed: usize,
    pub regressions: Vec<String>,
    pub improvements: Vec<String>,
    pub left_source: SourceStamp,
    pub right_source: SourceStamp,
}
impl BenchComparison {
    pub fn compare(left: &BenchReport, right: &BenchReport) -> Result<Self> {
        left.validate_structure()?;
        right.validate_structure()?;
        ensure!(
            left.suite_sha256 == right.suite_sha256
                && left.mode == right.mode
                && left.repetitions == right.repetitions
                && left.host_parameters == right.host_parameters
                && left.source.grader_rustc == right.source.grader_rustc,
            "benchmark plans are not comparable"
        );
        let mut compared = Self {
            selected: left.cases.len(),
            left_passed: left.summary.passed,
            right_passed: right.summary.passed,
            both_passed: 0,
            regressions: Vec::new(),
            improvements: Vec::new(),
            left_source: left.source.clone(),
            right_source: right.source.clone(),
        };
        ensure!(
            left.cases.len() == right.cases.len(),
            "different selected case counts"
        );
        for (a, b) in left.cases.iter().zip(&right.cases) {
            ensure!(
                a.id == b.id && a.repetition == b.repetition,
                "different selected cases or order"
            );
            ensure!(
                a.input_sha256.is_some() && a.input_sha256 == b.input_sha256,
                "actual model/config/resource/workspace inputs differ or are unknown"
            );
            let ap = a.verdict == crate::EvalVerdict::Passed;
            let bp = b.verdict == crate::EvalVerdict::Passed;
            let key = format!("{}#{}", a.id, a.repetition);
            if ap && bp {
                compared.both_passed += 1;
            } else if ap {
                compared.regressions.push(key);
            } else if bp {
                compared.improvements.push(key);
            }
        }
        Ok(compared)
    }
}

fn read_artifact(storage: &ArchiveRoot, artifact: &BenchArtifact) -> Result<Vec<u8>> {
    ensure!(
        artifact.bytes <= 64 * 1024 * 1024,
        "artifact byte budget exceeded"
    );
    let bytes = storage.read(&artifact.path, artifact.bytes)?;
    ensure!(
        bytes.len() == artifact.bytes && crate::bounds::sha256(&bytes) == artifact.sha256,
        "benchmark artifact fingerprint mismatch"
    );
    Ok(bytes)
}
