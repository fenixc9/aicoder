use aicoder_core::{RunProgress, model::*, session::*};
use aicoder_runtime::session::JsonlSessionRepository;
use serde_json::json;
use std::path::Path;

fn each_store(test: impl Fn(&dyn SessionRepository, &Path)) {
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    test(&MemorySessionRepository::new(), workspace.path());
    test(
        &JsonlSessionRepository::new(storage.path()).unwrap(),
        workspace.path(),
    );
}
fn user(text: &str) -> Message {
    Message::text(MessageRole::User, text)
}
fn data(message: Message) -> SessionEntryData {
    SessionEntryData::Message { message }
}
fn calls() -> Message {
    Message {
        role: MessageRole::Assistant,
        content: ["a", "b"]
            .into_iter()
            .map(|id| ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: id.into(),
                    name: "inspect".into(),
                    arguments: json!({}),
                },
            })
            .collect(),
    }
}
fn result(id: &str) -> Message {
    Message {
        role: MessageRole::Tool,
        content: vec![ContentBlock::ToolResult {
            call_id: id.into(),
            content: json!({"ok":true}),
            is_error: false,
        }],
    }
}

#[test]
fn branches_preserve_all_entries_but_project_only_the_selected_ancestry() {
    each_store(|store, workspace| {
        let mut session = store.create(workspace).unwrap();
        store.append(&mut session, user("root")).unwrap();
        let root = session.metadata().leaf_id.clone().unwrap();
        store
            .append_entries(
                &mut session,
                vec![
                    SessionEntryData::ModelChanged {
                        provider: "p".into(),
                        model: "one".into(),
                    },
                    data(user("branch one")),
                ],
            )
            .unwrap();
        let one = session.metadata().leaf_id.clone().unwrap();
        store.checkout(&mut session, Some(&root)).unwrap();
        store
            .append_entries(
                &mut session,
                vec![
                    SessionEntryData::ModelChanged {
                        provider: "p".into(),
                        model: "two".into(),
                    },
                    SessionEntryData::Extension {
                        namespace: "fixture".into(),
                        data: json!({"state":2}),
                    },
                    SessionEntryData::Compaction {
                        strategy: "fixture".into(),
                        summary: "record only".into(),
                        preserve_from: Some(root.clone()),
                        usage: ModelUsage::zero(),
                    },
                    data(user("branch two")),
                ],
            )
            .unwrap();
        let mut reopened = store.open(&session.metadata().id).unwrap();
        assert_eq!(reopened.entries().len(), 7);
        assert_eq!(
            reopened.model_messages().unwrap(),
            vec![user("root"), user("branch two")]
        );
        assert_eq!(reopened.selected_model(), Some(("p", "two")));
        store.checkout(&mut reopened, Some(&one)).unwrap();
        let reopened = store.open(&session.metadata().id).unwrap();
        assert_eq!(reopened.selected_model(), Some(("p", "one")));
        assert_eq!(
            reopened.chat_messages(),
            vec![user("root"), user("branch one")]
        );
        assert_eq!(store.list(workspace).unwrap()[0].entry_count, 7);
        let mut root_view = store.open(&session.metadata().id).unwrap();
        store.checkout(&mut root_view, None).unwrap();
        assert!(
            store
                .open(&session.metadata().id)
                .unwrap()
                .messages()
                .is_empty()
        );
    });
}

#[test]
fn stale_writes_invalid_batches_and_foreign_compaction_ancestors_are_atomic() {
    each_store(|store, workspace| {
        let mut session = store.create(workspace).unwrap();
        let mut stale = session.clone();
        store.append(&mut session, user("first")).unwrap();
        assert!(
            store
                .append(&mut stale, user("stale"))
                .unwrap_err()
                .downcast_ref::<SessionConflict>()
                .is_some()
        );
        assert!(stale.messages().is_empty());
        let before = session.metadata().clone();
        let first = session.metadata().leaf_id.clone().unwrap();
        let invalid = SessionTransaction {
            entries: vec![
                SessionEntry {
                    id: "valid".into(),
                    parent_id: Some(first.clone()),
                    created_at: 1,
                    data: data(user("must not persist")),
                },
                SessionEntry {
                    id: "invalid".into(),
                    parent_id: Some("missing".into()),
                    created_at: 1,
                    data: data(user("invalid")),
                },
            ],
            leaf_id: Some("invalid".into()),
        };
        assert!(store.commit(&mut session, invalid).is_err());
        assert_eq!(session.metadata(), &before);
        assert_eq!(store.open(&before.id).unwrap().messages().len(), 1);
        store.append(&mut session, user("old branch")).unwrap();
        let other = session.metadata().leaf_id.clone().unwrap();
        store.checkout(&mut session, Some(&first)).unwrap();
        assert!(
            store
                .append_entries(
                    &mut session,
                    vec![SessionEntryData::Compaction {
                        strategy: "fixture".into(),
                        summary: "bad".into(),
                        preserve_from: Some(other),
                        usage: ModelUsage::zero()
                    }]
                )
                .is_err()
        );
        assert!(store.checkout(&mut session, Some("missing")).is_err());
    });
}

#[test]
fn run_lease_allows_readers_and_owner_writes_but_blocks_other_writers_and_delete() {
    each_store(|store, workspace| {
        let mut session = store.create(workspace).unwrap();
        let id = session.metadata().id.clone();
        let lease = store.acquire_run(&mut session).unwrap();
        let mut reader = store.open(&id).unwrap();
        assert!(store.acquire_run(&mut reader).is_err());
        assert!(store.append(&mut reader, user("intruder")).is_err());
        assert!(store.delete(&id).is_err());
        store.append(&mut session, user("owner")).unwrap();
        assert_eq!(store.open(&id).unwrap().messages().len(), 1);
        drop(lease);
        let mut reopened = store.open(&id).unwrap();
        let next = store.acquire_run(&mut reopened).unwrap();
        assert!(store.append(&mut session, user("expired owner")).is_err());
        drop(next);
        store.delete(&id).unwrap();
    });
}

#[test]
fn interrupted_tool_units_are_projected_in_call_order_without_modifying_the_log() {
    each_store(|store, workspace| {
        let mut session = store.create(workspace).unwrap();
        store
            .append_entries(
                &mut session,
                vec![
                    SessionEntryData::TurnStart {
                        turn_id: "t".into(),
                    },
                    data(user("work")),
                    data(calls()),
                    data(result("b")),
                ],
            )
            .unwrap();
        let revision = session.metadata().revision;
        let projected = session.model_messages().unwrap();
        assert_eq!(projected[2].tool_result().unwrap().0, "a");
        assert!(projected[2].tool_result().unwrap().2);
        assert_eq!(
            projected[2].tool_result().unwrap().1["error"]["code"],
            "execution_interrupted"
        );
        assert_eq!(projected[3], result("b"));
        assert_eq!(projected.last().unwrap().role, MessageRole::System);
        assert_eq!(
            store
                .open(&session.metadata().id)
                .unwrap()
                .metadata()
                .revision,
            revision
        );
        assert_eq!(session.chat_messages().len(), 3);
        let mut record = TurnRecord::recovered();
        record.partial_response = Some(aicoder_core::run::PartialModelResponse {
            text: "UNTRUSTED_PARTIAL".into(),
            ..Default::default()
        });
        store
            .append_entries(
                &mut session,
                vec![SessionEntryData::TurnEnd {
                    turn_id: "t".into(),
                    record: Box::new(record),
                }],
            )
            .unwrap();
        assert!(session.unfinished_turn().is_none());
        assert!(
            !session
                .model_messages()
                .unwrap()
                .iter()
                .any(|m| m.text_content().contains("UNTRUSTED_PARTIAL"))
        );
    });
}

#[test]
fn complete_turns_require_paired_tools_and_matching_turn_markers() {
    each_store(|store, workspace| {
        let mut session = store.create(workspace).unwrap();
        assert!(
            store
                .append_entries(
                    &mut session,
                    vec![SessionEntryData::TurnEnd {
                        turn_id: "missing".into(),
                        record: Box::new(TurnRecord::recovered())
                    }]
                )
                .is_err()
        );
        store
            .append_entries(
                &mut session,
                vec![
                    SessionEntryData::TurnStart {
                        turn_id: "t".into(),
                    },
                    data(calls()),
                ],
            )
            .unwrap();
        assert!(
            store
                .append_entries(
                    &mut session,
                    vec![SessionEntryData::TurnStart {
                        turn_id: "nested".into()
                    }]
                )
                .is_err()
        );
        store
            .append_entries(
                &mut session,
                vec![SessionEntryData::TurnEnd {
                    turn_id: "t".into(),
                    record: Box::new(TurnRecord::from_progress(&RunProgress::new(vec![]), None)),
                }],
            )
            .unwrap();
        assert!(session.model_messages().is_err());
    });
}

#[test]
fn out_of_order_results_are_reordered_and_orphan_or_duplicate_results_are_rejected() {
    each_store(|store, workspace| {
        let mut session = store.create(workspace).unwrap();
        store
            .append_entries(
                &mut session,
                vec![data(calls()), data(result("b")), data(result("a"))],
            )
            .unwrap();
        let context = session.model_messages().unwrap();
        assert_eq!(context[1], result("a"));
        assert_eq!(context[2], result("b"));
        store.append(&mut session, result("a")).unwrap();
        assert!(session.model_messages().is_err());
        let mut orphan = store.create(workspace).unwrap();
        store.append(&mut orphan, result("x")).unwrap();
        assert!(orphan.model_messages().is_err());
    });
}

#[test]
fn external_repositories_can_construct_validate_and_fold_domain_snapshots() {
    let metadata = SessionMetadata {
        version: SESSION_VERSION,
        id: "custom-store-id".into(),
        cwd: std::env::temp_dir(),
        created_at: 1,
        updated_at: 1,
        title: None,
        leaf_id: None,
        revision: 0,
    };
    let mut session = Session::from_entries(metadata, vec![]).unwrap();
    let transaction = SessionTransaction {
        entries: vec![SessionEntry {
            id: "custom-entry".into(),
            parent_id: None,
            created_at: 2,
            data: data(user("custom storage")),
        }],
        leaf_id: Some("custom-entry".into()),
    };
    session.validate_transaction(&transaction).unwrap();
    assert!(
        session
            .apply_transaction(transaction.clone(), 2, 2)
            .is_err()
    );
    session.apply_transaction(transaction, 1, 2).unwrap();
    session.attach_run_token(Some("ephemeral lease".into()));
    assert_eq!(session.run_token(), Some("ephemeral lease"));
    let stored = serde_json::to_vec(&(session.metadata(), session.entries())).unwrap();
    let (metadata, entries): (SessionMetadata, Vec<SessionEntry>) =
        serde_json::from_slice(&stored).unwrap();
    let recovered = Session::from_entries(metadata, entries).unwrap();
    assert!(recovered.run_token().is_none());
    assert_eq!(recovered.chat_messages(), vec![user("custom storage")]);
    assert_eq!(recovered.metadata().revision, 1);
}
