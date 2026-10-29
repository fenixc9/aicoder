use aicoder_core::{model::*, session::*};
use serde_json::json;
fn message(text: &str) -> Message {
    Message::text(MessageRole::User, text)
}
#[test]
fn navigation_preserves_branches_global_names_labels_and_raw_messages() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append(&mut session, message("common")).unwrap();
    let common = session.metadata().leaf_id.clone().unwrap();
    repo.append(&mut session, message("original branch"))
        .unwrap();
    let old = session.metadata().leaf_id.clone().unwrap();
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let fork = nav
        .branch(&session.metadata().id, Some(&common), Some("experiment"))
        .unwrap();
    assert!(fork.run_token().is_none());
    assert_eq!(fork.messages().len(), 1);
    assert_eq!(fork.entries().len(), 3);
    let branch = fork.metadata().leaf_id.clone().unwrap();
    let renamed = nav.rename(&session.metadata().id, "project notes").unwrap();
    assert_eq!(renamed.metadata().title.as_deref(), Some("project notes"));
    let labelled = nav
        .label(&session.metadata().id, &old, Some("stable"))
        .unwrap();
    assert_eq!(labelled.resolve_entry("stable").unwrap(), old);
    assert_eq!(labelled.resolve_entry("experiment").unwrap(), branch);
    let restored = nav
        .checkout(&session.metadata().id, Some("stable"))
        .unwrap();
    assert_eq!(restored.metadata().title.as_deref(), Some("project notes"));
    assert_eq!(restored.messages().len(), 2);
    assert_eq!(restored.labels().len(), 2);
    assert_eq!(restored.entries().len(), labelled.entries().len());
    let unlabelled = nav.label(&session.metadata().id, &old, None).unwrap();
    assert!(unlabelled.resolve_entry("stable").is_err());
    assert!(unlabelled.resolve_entry(&old).is_ok());
    let empty = nav.checkout(&session.metadata().id, None).unwrap();
    assert!(empty.messages().is_empty());
    assert_eq!(empty.metadata().title.as_deref(), Some("project notes"));
}
#[test]
fn duplicate_labels_foreign_targets_and_active_leases_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append(&mut session, message("one")).unwrap();
    let one = session.metadata().leaf_id.clone().unwrap();
    repo.append(&mut session, message("two")).unwrap();
    let two = session.metadata().leaf_id.clone().unwrap();
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    nav.label(&session.metadata().id, &one, Some("duplicate"))
        .unwrap();
    let mut session = nav
        .label(&session.metadata().id, &two, Some("duplicate"))
        .unwrap();
    assert!(session.resolve_entry("duplicate").is_err());
    let before = session.metadata().clone();
    assert!(nav.label(&before.id, "missing", Some("no")).is_err());
    assert!(nav.rename(&before.id, "bad\nname").is_err());
    assert!(
        SessionNavigator::new(&repo, other.path())
            .unwrap()
            .checkout(&before.id, Some(&one))
            .is_err()
    );
    assert_eq!(repo.open(&before.id).unwrap().metadata(), &before);
    let lease = repo.acquire_run(&mut session).unwrap();
    assert!(nav.rename(&before.id, "busy").is_err());
    assert!(nav.branch(&before.id, None, None).is_err());
    assert!(nav.recover(&before.id).is_err());
    drop(lease);
    assert!(nav.rename(&before.id, "idle").is_ok());
}
#[test]
fn bounded_tree_pages_are_revision_bound_and_show_selected_ancestry() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    for i in 0..7 {
        repo.append(&mut session, message(&format!("message {i}")))
            .unwrap();
    }
    let id = session.metadata().id.clone();
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let root_entry = session.entries()[0].id.clone();
    let fork = nav.branch(&id, Some(&root_entry), Some("fork")).unwrap();
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let page = fork.tree_page(2, cursor.as_ref()).unwrap();
        assert!(page.rows.len() <= 2);
        rows.extend(page.rows);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(rows.len(), fork.entries().len());
    assert_eq!(rows.iter().filter(|row| row.tip).count(), 2);
    assert_eq!(rows.iter().filter(|row| row.on_selected_branch).count(), 2);
    assert_eq!(rows.iter().filter(|row| row.selected).count(), 1);
    assert_eq!(rows.last().unwrap().label.as_deref(), Some("fork"));
    let cursor = fork.tree_page(1, None).unwrap().next.unwrap();
    let changed = nav.rename(&id, "renamed").unwrap();
    assert!(changed.tree_page(1, Some(&cursor)).is_err());
    assert!(changed.tree_page(0, None).is_err());
}
#[test]
fn search_continues_across_large_utf8_blocks_without_reading_opaque_payloads() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    let text = format!("{}边界needle终点", "汉".repeat(90000));
    repo.append(&mut session, message(&text)).unwrap();
    repo.append(
        &mut session,
        Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::ProviderData {
                    provider: "private".into(),
                    data: json!({"needle":"OPAQUE_ONLY"}),
                },
                ContentBlock::Reasoning {
                    text: "REASONING_ONLY".into(),
                },
            ],
        },
    )
    .unwrap();
    let mut cursor = None;
    let mut found = Vec::new();
    let mut pages = 0;
    loop {
        let page = session
            .search_messages("needle", 1, cursor.as_ref())
            .unwrap();
        assert!(page.scanned_bytes <= 256 * 1024);
        assert!(page.scanned_entries <= 512);
        found.extend(page.hits);
        cursor = page.next;
        pages += 1;
        if cursor.is_none() {
            break;
        }
        assert!(pages < 5);
    }
    assert!(pages >= 2);
    assert_eq!(found.len(), 1);
    assert!(found[0].snippet.contains("边界needle终点"));
    assert!(
        session
            .search_messages("OPAQUE_ONLY", 10, None)
            .unwrap()
            .hits
            .is_empty()
    );
    assert!(
        session
            .search_messages("REASONING_ONLY", 10, None)
            .unwrap()
            .hits
            .is_empty()
    );
    let cursor = session
        .search_messages("absent", 1, None)
        .unwrap()
        .next
        .unwrap();
    assert!(
        session
            .search_messages("different", 1, Some(&cursor))
            .is_err()
    );
    repo.append(&mut session, message("later")).unwrap();
    assert!(session.search_messages("absent", 1, Some(&cursor)).is_err());
}
#[test]
fn search_bounds_metadata_only_scans_and_recovers_after_empty_pages() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append_entries(
        &mut session,
        (0..600)
            .map(|_| SessionEntryData::Extension {
                namespace: "fixture".into(),
                data: json!({}),
            })
            .chain([SessionEntryData::Message {
                message: message("target"),
            }])
            .collect(),
    )
    .unwrap();
    let first = session.search_messages("target", 10, None).unwrap();
    assert!(first.hits.is_empty());
    assert_eq!(first.scanned_entries, 512);
    let second = session
        .search_messages("target", 10, first.next.as_ref())
        .unwrap();
    assert_eq!(second.hits.len(), 1);
    assert!(second.next.is_none());
}
#[test]
fn explicit_recovery_is_idempotent_and_never_records_synthetic_tool_results() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::TurnStart {
                turn_id: "vanished".into(),
            },
            SessionEntryData::Message {
                message: Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "pending".into(),
                            name: "work".into(),
                            arguments: json!({}),
                        },
                    }],
                },
            },
        ],
    )
    .unwrap();
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let recovered = nav.recover(&session.metadata().id).unwrap();
    assert!(recovered.unfinished_turn().is_none());
    assert_eq!(recovered.messages().len(), 1);
    assert!(
        recovered
            .selected_branch()
            .model_context()
            .unwrap()
            .messages
            .iter()
            .any(|m| m.tool_result().is_some())
    );
    let second = nav.recover(&session.metadata().id).unwrap();
    assert_eq!(second.metadata(), recovered.metadata());
    assert!(
        second
            .messages()
            .iter()
            .all(|m| m.message.role != MessageRole::Tool)
    );
}

#[test]
fn label_quota_validation_is_atomic_and_removal_frees_a_slot() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    for i in 0..257 {
        repo.append(&mut session, message(&format!("entry {i}")))
            .unwrap();
    }
    let ids = session
        .entries()
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    repo.append_entries(
        &mut session,
        ids[..256]
            .iter()
            .map(|id| {
                NavigationRecord::Label {
                    entry_id: id.clone(),
                    label: Some("label".into()),
                }
                .data()
            })
            .collect(),
    )
    .unwrap();
    assert_eq!(session.labels().len(), 256);
    let before = session.metadata().clone();
    assert!(
        repo.append_entries(
            &mut session,
            vec![
                NavigationRecord::Rename {
                    title: "must roll back".into()
                }
                .data(),
                NavigationRecord::Label {
                    entry_id: ids[256].clone(),
                    label: Some("excess".into())
                }
                .data()
            ]
        )
        .is_err()
    );
    assert_eq!(session.metadata(), &before);
    repo.append_entries(
        &mut session,
        vec![
            NavigationRecord::Label {
                entry_id: ids[0].clone(),
                label: None,
            }
            .data(),
            NavigationRecord::Label {
                entry_id: ids[256].clone(),
                label: Some("replacement".into()),
            }
            .data(),
        ],
    )
    .unwrap();
    assert_eq!(session.labels().len(), 256);
}
