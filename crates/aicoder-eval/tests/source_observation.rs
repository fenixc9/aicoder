use aicoder_eval::benchmark::{BenchReport, SourceStamp};
use std::{fs, path::Path, process::Command};

#[test]
fn prelaunch_source_observations_bind_executable_platform_and_compiler() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let observed = SourceStamp::capture(source).unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.json");
    fs::write(&path, serde_json::to_vec(&observed).unwrap()).unwrap();
    assert_eq!(SourceStamp::load_observation(&path).unwrap(), observed);
    for changed in [
        SourceStamp {
            executable_sha256: None,
            ..observed.clone()
        },
        SourceStamp {
            executable_sha256: Some("0".repeat(64)),
            ..observed.clone()
        },
        SourceStamp {
            platform: "different/platform".into(),
            ..observed.clone()
        },
        SourceStamp {
            grader_rustc: Some("different compiler".into()),
            ..observed
        },
    ] {
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(SourceStamp::load_observation(&path).is_err());
    }
}

#[test]
fn source_observation_inputs_are_bounded_regular_files_without_value_echoes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.json");
    fs::write(&path, br#"{"private":"must-not-appear"}"#).unwrap();
    assert!(
        !SourceStamp::load_observation(&path)
            .unwrap_err()
            .to_string()
            .contains("must-not-appear")
    );
    fs::write(&path, vec![b' '; 16 * 1024 + 1]).unwrap();
    assert!(SourceStamp::load_observation(&path).is_err());
    #[cfg(unix)]
    {
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(SourceStamp::load_observation(&link).is_err());
    }
}

#[test]
fn cli_prelaunch_capture_runs_in_a_source_free_workspace_and_rejects_mismatches_before_admission() {
    let binary = env!("CARGO_BIN_EXE_aicoder-bench");
    let root = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let observed = Command::new(binary)
        .args(["observe-source", "--source-root"])
        .arg(source)
        .output()
        .unwrap();
    assert!(observed.status.success());
    let path = root.path().join("source.json");
    fs::write(&path, &observed.stdout).unwrap();
    let workspace = root.path().join("source-free");
    fs::create_dir(&workspace).unwrap();
    let output = root.path().join("run");
    let run = Command::new(binary)
        .args(["run", "--source-root"])
        .arg(&workspace)
        .arg("--source-observation")
        .arg(&path)
        .arg("--output")
        .arg(&output)
        .args(["--case", "retry_config"])
        .output()
        .unwrap();
    assert!(run.status.success(), "{:?}", run.stderr);
    let report = BenchReport::verify(&output).unwrap();
    let expected: SourceStamp = serde_json::from_slice(&observed.stdout).unwrap();
    assert_eq!(report.source, expected);
    assert_eq!(report.summary.passed, 1);
    assert_eq!(
        report.host_parameters["source_observation"],
        "explicit_host_prelaunch_observation"
    );
    let mut wrong = expected;
    wrong.executable_sha256 = Some("0".repeat(64));
    fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
    let failed = root.path().join("must-not-start");
    let attempt = Command::new(binary)
        .args(["run", "--live", "--model", "never-called", "--source-root"])
        .arg(&workspace)
        .arg("--source-observation")
        .arg(&path)
        .arg("--output")
        .arg(&failed)
        .output()
        .unwrap();
    assert!(!attempt.status.success());
    assert!(!failed.exists());
}
