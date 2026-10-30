use aicoder_core::{
    model::{Message, MessageRole},
    session::SessionRepository,
};
use aicoder_runtime::session::JsonlSessionRepository;
use serde_json::Value;
use std::{fs, path::Path, process::Command};
fn command(workspace: &Path, state: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aicoder"));
    c.env_clear()
        .env("AICODER_HOME", state)
        .env("AICODER_API_BASE", "http://127.0.0.1:1/v1")
        .env("AICODER_PROXY", "disabled")
        .env("AICODER_MODEL", "fixture")
        .args(["--no-config", "--no-resources", "--workspace"])
        .arg(workspace);
    c
}
#[test]
fn local_export_and_explicit_directory_publication_use_no_model_credentials() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(state.path().join("sessions")).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append(
        &mut session,
        Message::text(MessageRole::User, "private <script>example</script>"),
    )
    .unwrap();
    let source = fs::read(session.path().unwrap()).unwrap();
    let path = output.path().join("session.json");
    let response = command(workspace.path(), state.path())
        .args(["export-session", &session.metadata().id, "--output"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        response.status.success(),
        "{}",
        String::from_utf8_lossy(&response.stderr)
    );
    let receipt: Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(receipt["export"]["canonical_entries_complete"], true);
    assert_eq!(receipt["export"]["entries"], 1);
    assert!(!String::from_utf8_lossy(&response.stdout).contains("private <script>"));
    let data: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        data["entries"][0]["entry"]["data"]["message"]["content"][0]["text"],
        "private <script>example</script>"
    );
    let share = command(workspace.path(), state.path())
        .args(["share-session", &session.metadata().id, "--directory"])
        .arg(output.path())
        .args(["--base-url", "https://example.invalid/share/"])
        .output()
        .unwrap();
    assert!(
        share.status.success(),
        "{}",
        String::from_utf8_lossy(&share.stderr)
    );
    let receipt: Value = serde_json::from_slice(&share.stdout).unwrap();
    assert_eq!(receipt["export"]["format"], "html");
    assert!(
        receipt["url"]
            .as_str()
            .unwrap()
            .starts_with("https://example.invalid/share/")
    );
    let html = fs::read_to_string(receipt["path"].as_str().unwrap()).unwrap();
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    assert_eq!(fs::read(session.path().unwrap()).unwrap(), source);
}
#[test]
fn failures_never_replace_existing_output_or_export_a_foreign_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(state.path().join("sessions")).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append(&mut session, Message::text(MessageRole::User, "recorded"))
        .unwrap();
    let path = output.path().join("keep.json");
    fs::write(&path, b"keep").unwrap();
    let response = command(workspace.path(), state.path())
        .args(["export-session", &session.metadata().id, "--output"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!response.status.success());
    assert_eq!(fs::read(&path).unwrap(), b"keep");
    assert!(response.stdout.is_empty());
    assert!(String::from_utf8_lossy(&response.stderr).contains("visible=false"));
    let absent = output.path().join("absent.json");
    for root in [workspace.path(), other.path()] {
        let mut cmd = command(root, state.path());
        cmd.args(["export-session", &session.metadata().id, "--output"])
            .arg(&absent);
        if root == workspace.path() {
            cmd.args(["--max-bytes", "10"]);
        }
        let response = cmd.output().unwrap();
        assert!(!response.status.success());
        assert!(!absent.exists());
        assert!(response.stdout.is_empty());
    }
    assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
}
