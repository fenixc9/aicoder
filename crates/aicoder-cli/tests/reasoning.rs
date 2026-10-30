use aicoder_core::{
    model::{Message, MessageRole},
    session::*,
};
use aicoder_runtime::session::JsonlSessionRepository;
use serde_json::{Value, json};
use std::{fs, process::Command};
#[test]
fn metadata_commands_validate_without_credentials_and_fail_without_writing() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let config = state.path().join("settings.json");
    fs::write(&config,json!({"version":1,"profiles":{"work":{"base_url":"http://127.0.0.1:1/v1","api_key_env":"FIXTURE_KEY","provider_id":"fixture","model":"m1","proxy":"disabled","models":[{"provider":"fixture","id":"m1","capabilities":{"tools":true,"reasoning_efforts":["none","low","high"]}}]}},"settings":{"provider_profile":"work"}}).to_string()).unwrap();
    let repo = JsonlSessionRepository::new(state.path().join("sessions")).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::ModelChanged {
                provider: "fixture".into(),
                model: "m1".into(),
            },
            SessionEntryData::Message {
                message: Message::text(MessageRole::User, "retained"),
            },
        ],
    )
    .unwrap();
    let run = |level: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aicoder"));
        command
            .env_clear()
            .env("AICODER_HOME", state.path())
            .args(["--user-config"])
            .arg(&config)
            .args(["--no-resources", "--workspace"])
            .arg(workspace.path())
            .args(["set-session-reasoning", &session.metadata().id, level]);
        command.output().unwrap()
    };
    let selected = run("low");
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    let report: Value = serde_json::from_slice(&selected.stdout).unwrap();
    assert_eq!(report["reasoning_effort"], "low");
    let before = fs::read(session.path().unwrap()).unwrap();
    let unsupported = run("max");
    assert!(!unsupported.status.success());
    assert_eq!(fs::read(session.path().unwrap()).unwrap(), before);
    assert!(unsupported.stdout.is_empty());
    let cleared = run("default");
    assert!(cleared.status.success());
    assert_eq!(
        repo.open(&session.metadata().id)
            .unwrap()
            .reasoning_selection()
            .unwrap()
            .unwrap()
            .effort,
        None
    );
}
