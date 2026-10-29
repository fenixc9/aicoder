use aicoder_core::artifacts::*;
use aicoder_runtime::{ConfigDiscovery, Runtime};
use std::{collections::BTreeMap, process::Command, sync::Arc};
#[tokio::test]
async fn cli_reads_existing_artifact_without_model_credentials_and_rejects_other_workspace() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        Default::default(),
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::from([(
            "AICODER_HOME".into(),
            state.path().to_string_lossy().into_owned(),
        )])),
    )
    .unwrap();
    let store = runtime
        .artifact_store_for(workspace.path())
        .unwrap()
        .unwrap();
    let reference = store_bytes(store.as_ref(), "text/plain", b"0123456789", true)
        .await
        .unwrap();
    let run = |workspace: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_aicoder"))
            .env_clear()
            .env("AICODER_HOME", state.path())
            .args(["--no-config", "--no-resources", "--workspace"])
            .arg(workspace)
            .args([
                "read-artifact",
                &reference.id,
                "--offset",
                "3",
                "--max-bytes",
                "4",
            ])
            .output()
            .unwrap()
    };
    let output = run(workspace.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let page: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(page["content"], "3456");
    assert_eq!(page["next_offset"], 7);
    assert_eq!(page["artifact"]["id"], reference.id);
    let output = run(other.path());
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not_found"));
    let output = Command::new(env!("CARGO_BIN_EXE_aicoder"))
        .env_clear()
        .env("AICODER_HOME", state.path())
        .args([
            "--no-config",
            "--no-resources",
            "--no-session",
            "--workspace",
        ])
        .arg(workspace.path())
        .arg("config")
        .output()
        .unwrap();
    assert!(output.status.success());
    let config: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(config["session_dir"].is_null());
}
