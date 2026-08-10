use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::SweBenchPrediction;

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
}

impl SweBenchHarnessConfig {
    pub fn new(
        dataset_name: impl Into<String>,
        predictions_path: impl Into<PathBuf>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            python: PathBuf::from("python"),
            dataset_name: dataset_name.into(),
            split: "test".to_string(),
            predictions_path: predictions_path.into(),
            instance_ids: Vec::new(),
            max_workers: 1,
            run_id: run_id.into(),
            timeout_seconds: 1_800,
            report_dir: PathBuf::from("."),
            modal: false,
        }
    }

    pub fn command(&self) -> Result<Command> {
        ensure!(
            self.max_workers > 0,
            "SWE-bench harness workers must be positive"
        );
        ensure!(
            !self.run_id.trim().is_empty(),
            "SWE-bench harness run id cannot be empty"
        );
        ensure!(
            self.predictions_path.is_file(),
            "SWE-bench predictions do not exist: {}",
            self.predictions_path.display()
        );
        fs::create_dir_all(&self.report_dir).with_context(|| {
            format!(
                "Failed to create SWE-bench harness report directory {}",
                self.report_dir.display()
            )
        })?;
        let predictions = self
            .predictions_path
            .canonicalize()
            .with_context(|| format!("Failed to resolve {}", self.predictions_path.display()))?;
        let report_dir = self.report_dir.canonicalize().with_context(|| {
            format!(
                "Failed to resolve report directory {}",
                self.report_dir.display()
            )
        })?;
        let mut command = Command::new(&self.python);
        command
            .current_dir(report_dir)
            .args(["-m", "swebench.harness.run_evaluation"])
            .arg("--dataset_name")
            .arg(&self.dataset_name)
            .arg("--split")
            .arg(&self.split)
            .arg("--predictions_path")
            .arg(predictions)
            .arg("--max_workers")
            .arg(self.max_workers.to_string())
            .arg("--run_id")
            .arg(&self.run_id)
            .arg("--timeout")
            .arg(self.timeout_seconds.to_string())
            .arg("--report_dir")
            .arg(".")
            .arg("--modal")
            .arg(self.modal.to_string());
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
        let path = path.as_ref();
        let content = fs::read(path)
            .with_context(|| format!("Failed to read harness report {}", path.display()))?;
        serde_json::from_slice(&content)
            .with_context(|| format!("Failed to parse harness report {}", path.display()))
    }

    pub fn resolved_rate(&self) -> f64 {
        if self.submitted_instances == 0 {
            0.0
        } else {
            self.resolved_instances as f64 / self.submitted_instances as f64
        }
    }
}

#[derive(Debug)]
pub struct SweBenchHarnessExecution {
    pub output: Output,
    pub report_path: Option<PathBuf>,
    pub report: Option<SweBenchHarnessReport>,
}

pub fn run_swebench_harness(config: &SweBenchHarnessConfig) -> Result<SweBenchHarnessExecution> {
    let predictions = load_predictions(&config.predictions_path)?;
    ensure!(
        !predictions.is_empty(),
        "SWE-bench predictions file is empty"
    );
    let model_name = predictions[0].model_name_or_path.replace('/', "__");
    ensure!(
        predictions
            .iter()
            .all(|prediction| prediction.model_name_or_path == predictions[0].model_name_or_path),
        "Official SWE-bench harness report requires one model_name_or_path per predictions file"
    );
    let report_path = config
        .report_dir
        .join(format!("{model_name}.{}.json", config.run_id));
    let output = config
        .command()?
        .output()
        .context("Failed to start official SWE-bench evaluation harness")?;
    if !output.status.success() {
        anyhow::bail!(
            "SWE-bench harness failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let (report_path, report) = if report_path.is_file() {
        let report = SweBenchHarnessReport::load(&report_path)?;
        (Some(report_path), Some(report))
    } else {
        (None, None)
    };
    Ok(SweBenchHarnessExecution {
        output,
        report_path,
        report,
    })
}

fn load_predictions(path: &Path) -> Result<Vec<SweBenchPrediction>> {
    let content =
        fs::read(path).with_context(|| format!("Failed to read predictions {}", path.display()))?;
    serde_json::from_slice(&content)
        .with_context(|| format!("Failed to parse predictions {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_official_aggregate_report_and_calculates_rate() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "total_instances": 3,
                "submitted_instances": 2,
                "completed_instances": 1,
                "resolved_instances": 1,
                "unresolved_instances": 1,
                "empty_patch_instances": 0,
                "error_instances": 0,
                "completed_ids": ["a", "b"],
                "resolved_ids": ["a"],
                "unresolved_ids": ["b"],
                "schema_version": 2
            }))
            .unwrap(),
        )
        .unwrap();
        let report = SweBenchHarnessReport::load(path).unwrap();
        assert_eq!(report.schema_version, 2);
        assert!((report.resolved_rate() - 0.5).abs() < f64::EPSILON);
    }
}
