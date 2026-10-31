use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
fn cli(home: &Path, workspace: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aicoder"))
        .env_clear()
        .env("AICODER_HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(workspace)
        .args([
            "--no-config",
            "--no-parent-context",
            "--workspace",
            workspace.to_str().unwrap(),
        ])
        .args(args)
        .output()
        .unwrap()
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn metadata_install_pin_reload_and_remove_work_without_credentials_or_requests() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let path = source.path().to_str().unwrap();
    let manifest = json!({"schema_version":1,"id":"demo","version":"1","files":["rules.md"],"permissions":{"instructions":true},"resources":{"instructions":["rules.md"]}});
    fs::write(
        source.path().join("aicoder-package.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(source.path().join("rules.md"), "RULE_ONE").unwrap();
    assert_eq!(
        success(cli(
            home.path(),
            workspace.path(),
            &["packages", "inspect", path]
        ))["id"],
        "demo"
    );
    assert!(!home.path().join("packages").exists());
    let record = success(cli(
        home.path(),
        workspace.path(),
        &["packages", "install", path],
    ));
    let pin = &record["pin"];
    let lock = success(cli(home.path(), workspace.path(), &["packages", "lock"]));
    assert_eq!(lock["pins"][0], *pin);
    let lock_path = workspace.path().join("packages.lock.json");
    fs::write(&lock_path, serde_json::to_vec(&lock).unwrap()).unwrap();
    let resources = success(cli(home.path(), workspace.path(), &["resources"]));
    assert_eq!(resources["packages"][0], *pin);
    success(cli(
        home.path(),
        workspace.path(),
        &["packages", "disable", "demo"],
    ));
    let resources = success(cli(home.path(), workspace.path(), &["resources"]));
    assert!(resources.get("packages").is_none());
    let resources = success(cli(
        home.path(),
        workspace.path(),
        &["--package-lock", lock_path.to_str().unwrap(), "resources"],
    ));
    assert_eq!(resources["packages"][0], *pin);
    success(cli(
        home.path(),
        workspace.path(),
        &[
            "packages",
            "select",
            "demo",
            "1",
            pin["sha256"].as_str().unwrap(),
        ],
    ));
    success(cli(
        home.path(),
        workspace.path(),
        &["packages", "enable", "demo"],
    ));
    let resources = success(cli(
        home.path(),
        workspace.path(),
        &["--no-packages", "resources"],
    ));
    assert!(resources.get("packages").is_none());
    success(cli(
        home.path(),
        workspace.path(),
        &["packages", "remove", "demo"],
    ));
    let missing = cli(
        home.path(),
        workspace.path(),
        &["--package-lock", lock_path.to_str().unwrap(), "resources"],
    );
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(
        success(cli(home.path(), workspace.path(), &["packages", "list"]))["installed"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn oversized_and_untrusted_lock_documents_fail_without_emitting_partial_json() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("oversized.json");
    fs::write(&path, vec![b' '; 32769]).unwrap();
    let result = cli(
        home.path(),
        workspace.path(),
        &["--package-lock", path.to_str().unwrap(), "resources"],
    );
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    fs::write(
        &path,
        serde_json::to_vec(
            &json!({"version":1,"pins":[{"id":"untrusted","version":"1","sha256":"a".repeat(64)}]}),
        )
        .unwrap(),
    )
    .unwrap();
    let result = cli(
        home.path(),
        workspace.path(),
        &["--package-lock", path.to_str().unwrap(), "resources"],
    );
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
}
