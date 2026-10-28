use serde_json::{Value, json};
use std::{fs, process::Command};
#[test]
fn configuration_inspection_obeys_shared_precedence_without_credentials_or_session_writes() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(workspace.path().join(".aicoder")).unwrap();
    fs::write(
        workspace.path().join(".aicoder/settings.json"),
        json!({"version":1,"settings":{"model":"project-model","execution":{"max_rounds":4}}})
            .to_string(),
    )
    .unwrap();
    fs::write(
        workspace.path().join(".env"),
        "AICODER_API_KEY=must-not-be-implicitly-loaded\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aicoder"))
        .current_dir(workspace.path())
        .env_clear()
        .env("AICODER_HOME", home.path())
        .env("AICODER_MODEL", "environment-model")
        .env("AICODER_MAX_ROUNDS", "5")
        .args(["--max-rounds", "7", "config"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(config["provider"]["model"], "environment-model");
    assert_eq!(config["execution"]["max_rounds"], 7);
    assert_eq!(
        config["sources"]["execution.max_rounds"]["kind"],
        "explicit"
    );
    assert!(config["provider"].get("api_key").is_none());
    assert!(!home.path().join("sessions").exists());
}
#[test]
fn explicit_environment_files_are_nonmutating_fallbacks_and_snapshots_exclude_their_credentials() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let file = workspace.path().join("trusted.env");
    fs::write(&file,"AICODER_MODEL=file-model\nAICODER_MAX_ROUNDS=6\nAICODER_API_KEY_ENV=PRIVATE_TOKEN\nPRIVATE_TOKEN=credential-sentinel\nAICODER_API_BASE=https://unused.invalid/v1\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aicoder"))
        .current_dir(workspace.path())
        .env_clear()
        .env("AICODER_HOME", home.path())
        .env("OPENAI_MODEL", "process-model")
        .env("OPENAI_API_BASE", "https://process.invalid/v1")
        .arg("--env-file")
        .arg(&file)
        .arg("config")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(config["provider"]["model"], "process-model");
    assert_eq!(config["provider"]["base_url"], "https://process.invalid/v1");
    assert_eq!(config["execution"]["max_rounds"], 6);
    assert_eq!(
        config["sources"]["execution.max_rounds"]["kind"],
        "environment_file"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("credential-sentinel"));
}
