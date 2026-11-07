use aicoder_core::{
    model::{Message, MessageRole},
    session::SessionRepository,
};
use aicoder_runtime::session::JsonlSessionRepository;
use std::process::Command;

#[test]
fn cli_lists_branches_and_checks_out_an_entry_without_provider_configuration() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repository = JsonlSessionRepository::new(home.path().join("sessions")).unwrap();
    let mut session = repository.create(workspace.path()).unwrap();
    repository
        .append(&mut session, Message::text(MessageRole::User, "root"))
        .unwrap();
    let root = session.metadata().leaf_id.clone().unwrap();
    repository
        .append(
            &mut session,
            Message::text(MessageRole::Assistant, "old branch"),
        )
        .unwrap();
    let id = session.metadata().id.clone();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_aicoder"))
            .env_clear()
            .current_dir(workspace.path())
            .env("AICODER_HOME", home.path())
            .args(args)
            .output()
            .unwrap()
    };
    let tree = invoke(&["session-tree", &id, "--json"]);
    assert!(
        tree.status.success(),
        "{}",
        String::from_utf8_lossy(&tree.stderr)
    );
    let tree: serde_json::Value = serde_json::from_slice(&tree.stdout).unwrap();
    assert_eq!(tree["entries"].as_array().unwrap().len(), 2);
    assert!(invoke(&["checkout-session", &id, &root]).status.success());
    let reopened = repository.open(&id).unwrap();
    assert_eq!(reopened.messages().len(), 1);
    assert_eq!(reopened.entries().len(), 2);
    assert!(
        !invoke(&["checkout-session", &id, "invalid"])
            .status
            .success()
    );
    assert!(invoke(&["migrate-session", &id]).status.success());
    assert!(
        invoke(&["checkout-session", &id, "--root"])
            .status
            .success()
    );
    assert!(repository.open(&id).unwrap().messages().is_empty());
}

#[test]
fn cli_manages_metadata_pages_and_models_without_network_or_credentials() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(home.path().join("sessions")).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append(
        &mut session,
        Message::text(MessageRole::User, "needle common"),
    )
    .unwrap();
    let root = session.metadata().leaf_id.clone().unwrap();
    let id = session.metadata().id.clone();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_aicoder"))
            .env_clear()
            .env("AICODER_HOME", home.path())
            .current_dir(workspace.path())
            .args(["--no-config", "--no-resources"])
            .args(args)
            .output()
            .unwrap()
    };
    for args in [
        vec!["rename-session", &id, "renamed"],
        vec!["label-session", &id, &root, "stable"],
        vec![
            "branch-session",
            &id,
            "--from",
            "stable",
            "--label",
            "experiment",
        ],
        vec!["recover-session", &id],
        vec!["switch-session-model", &id, "gpt-test-model"],
    ] {
        let output = invoke(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = invoke(&["search-session", &id, "needle", "--limit", "1"]);
    assert!(output.status.success());
    let page: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(page["hits"][0]["entry_id"], root);
    let output = invoke(&["session-branches", &id, "--limit", "1"]);
    assert!(output.status.success());
    let page: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(page["rows"].as_array().unwrap().len(), 1);
    let cursor = serde_json::to_string(&page["next"]).unwrap();
    assert!(
        invoke(&["session-branches", &id, "--limit", "1", "--cursor", &cursor])
            .status
            .success()
    );
    let saved = repo.open(&id).unwrap();
    assert_eq!(saved.metadata().title.as_deref(), Some("renamed"));
    assert_eq!(saved.selected_model().unwrap().1, "gpt-test-model");
    assert_eq!(saved.messages().len(), 1);
    assert!(
        invoke(&["checkout-session", &id, "stable"])
            .status
            .success()
    );
}
#[test]
fn cli_fork_and_clone_publish_new_identities_without_mutating_the_source_or_calling_models() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(home.path().join("sessions")).unwrap();
    let mut s = repo.create(workspace.path()).unwrap();
    for (role, text) in [
        (MessageRole::User, "first"),
        (MessageRole::Assistant, "done"),
        (MessageRole::User, "restore draft"),
    ] {
        repo.append(&mut s, Message::text(role, text)).unwrap();
    }
    let path = s.path().unwrap().to_owned();
    let before = std::fs::read(&path).unwrap();
    for (command, expected, editor) in [("fork-session", 2, true), ("clone-session", 3, false)] {
        let result = Command::new(env!("CARGO_BIN_EXE_aicoder"))
            .env_clear()
            .env("AICODER_HOME", home.path())
            .current_dir(workspace.path())
            .args(["--no-config", "--no-resources", command, &s.metadata().id])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let reply: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let id = reply["session"]["id"].as_str().unwrap();
        assert_ne!(id, s.metadata().id);
        assert_eq!(repo.open(id).unwrap().messages().len(), expected);
        assert_eq!(reply["editor"].is_object(), editor);
        if editor {
            assert_eq!(reply["editor"]["text"], "restore draft");
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
