use std::{fs, process::Command};
#[test]
fn tui_can_inspect_the_same_runtime_settings_without_a_terminal_or_credentials() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(workspace.path().join(".aicoder")).unwrap();
    fs::write(
        workspace.path().join(".aicoder/settings.json"),
        r#"{"version":1,"settings":{"model":"file-model","execution":{"max_rounds":5}}}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aicoder-tui"))
        .current_dir(workspace.path())
        .env_clear()
        .env("AICODER_HOME", home.path())
        .args(["--show-config", "--model", "explicit-model"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(config["provider"]["model"], "explicit-model");
    assert_eq!(config["execution"]["max_rounds"], 5);
    assert!(!home.path().join("sessions").exists());
    assert!(!home.path().join("tui.log").exists());
}
