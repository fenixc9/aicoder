use aicoder_core::{model::*, session::*};
use aicoder_runtime::session::JsonlSessionRepository;
#[test]
fn jsonl_navigation_replays_global_metadata_and_preserves_old_branch_bytes() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append(&mut session, Message::text(MessageRole::User, "common"))
        .unwrap();
    let common = session.metadata().leaf_id.clone().unwrap();
    repo.append(
        &mut session,
        Message::text(MessageRole::Assistant, "original"),
    )
    .unwrap();
    let old = session.metadata().leaf_id.clone().unwrap();
    let original = std::fs::read(session.path().unwrap()).unwrap();
    let nav = SessionNavigator::new(&repo, workspace.path()).unwrap();
    nav.branch(&session.metadata().id, Some(&common), Some("experiment"))
        .unwrap();
    nav.rename(&session.metadata().id, "renamed globally")
        .unwrap();
    nav.label(&session.metadata().id, &old, Some("stable"))
        .unwrap();
    nav.checkout(&session.metadata().id, Some("stable"))
        .unwrap();
    let reopened = repo.open(&session.metadata().id).unwrap();
    assert_eq!(
        reopened.metadata().title.as_deref(),
        Some("renamed globally")
    );
    assert_eq!(reopened.messages().len(), 2);
    assert!(reopened.resolve_entry("experiment").is_ok());
    assert!(
        std::fs::read(reopened.path().unwrap())
            .unwrap()
            .starts_with(&original)
    );
    assert_eq!(reopened.metadata().version, 3);
    let mut stale = reopened.clone();
    nav.rename(&session.metadata().id, "newer").unwrap();
    assert!(
        repo.append(&mut stale, Message::text(MessageRole::User, "stale"))
            .is_err()
    );
}
#[test]
fn invalid_navigation_batches_leave_file_bytes_and_snapshot_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    let before = std::fs::read(session.path().unwrap()).unwrap();
    let metadata = session.metadata().clone();
    assert!(
        repo.append_entries(
            &mut session,
            vec![
                NavigationRecord::Rename {
                    title: "temporary".into()
                }
                .data(),
                NavigationRecord::Label {
                    entry_id: "foreign".into(),
                    label: Some("no".into())
                }
                .data()
            ]
        )
        .is_err()
    );
    assert_eq!(session.metadata(), &metadata);
    assert_eq!(std::fs::read(session.path().unwrap()).unwrap(), before);
}
