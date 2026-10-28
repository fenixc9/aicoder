use aicoder_core::{
    model::{Message, MessageRole},
    session::{JsonlSessionRepository, SessionRepository},
};
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
