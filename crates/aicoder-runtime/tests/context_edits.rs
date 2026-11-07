use aicoder_core::{model::*, session::*};
use aicoder_runtime::session::{JsonlSessionRepository, SessionLoadLimits};
use serde_json::json;
fn edit(target: &str, replacement: ContextReplacement) -> ArchiveNavigation {
    ArchiveNavigation::EditContext {
        edit: ContextEditRecord {
            version: 1,
            extension: "fixture".into(),
            hook: "edit".into(),
            target_id: target.into(),
            replacement,
        },
    }
}
fn seed(repo: &JsonlSessionRepository, workspace: &std::path::Path) -> Session {
    let mut s = repo.create(workspace).unwrap();
    for i in 0..12 {
        repo.append(
            &mut s,
            Message::text(MessageRole::User, format!("raw-{i} {}", "x".repeat(600))),
        )
        .unwrap();
    }
    s
}
fn limited(path: &std::path::Path, bytes: usize) -> JsonlSessionRepository {
    JsonlSessionRepository::with_limits(
        path,
        SessionLoadLimits {
            max_snapshot_bytes: 512,
            max_window_messages: 2,
            max_window_bytes: bytes,
            ..Default::default()
        },
    )
    .unwrap()
}
#[test]
fn cold_archive_reads_latest_real_edits_and_preserves_raw_pages_and_branch_views() {
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(storage.path()).unwrap();
    let s = seed(&writer, workspace.path());
    let id = s.metadata().id.clone();
    let target = s.entries().last().unwrap().id.clone();
    let original = std::fs::read(s.path().unwrap()).unwrap();
    let repo = limited(storage.path(), 4096);
    let nav = SessionNavigator::new(&repo, workspace.path()).unwrap();
    let prepared = nav
        .prepare(
            &id,
            edit(
                &target,
                ContextReplacement::Text("actual replacement".into()),
            ),
        )
        .unwrap();
    assert_eq!(std::fs::read(s.path().unwrap()).unwrap(), original);
    assert!(
        nav.prepare(&id, edit(&target, ContextReplacement::Omit))
            .is_err()
    ); // lease remains owned
    let changed = prepared.commit().unwrap();
    assert!(matches!(changed, RunSession::Window(_)));
    assert_eq!(
        changed
            .source_and_context()
            .unwrap()
            .1
            .last()
            .unwrap()
            .text_content(),
        "actual replacement"
    );
    assert!(
        std::fs::read(s.path().unwrap())
            .unwrap()
            .starts_with(&original)
    );
    let full = writer.open(&id).unwrap();
    assert!(
        full.messages()
            .last()
            .unwrap()
            .message
            .text_content()
            .starts_with("raw-11")
    );
    assert_eq!(
        full.selected_branch()
            .model_context()
            .unwrap()
            .messages
            .last()
            .unwrap()
            .text_content(),
        "actual replacement"
    );
    let fresh = limited(storage.path(), 4096);
    let reopened = fresh.open_run(&id).unwrap();
    assert_eq!(
        reopened
            .source_and_context()
            .unwrap()
            .1
            .last()
            .unwrap()
            .text_content(),
        "actual replacement"
    );
    let nav = SessionNavigator::new(&fresh, workspace.path()).unwrap();
    let omitted = nav
        .execute(&id, edit(&target, ContextReplacement::Omit))
        .unwrap();
    assert_eq!(omitted.source_and_context().unwrap().1.len(), 2); // archive header plus raw-10
    let restored = nav
        .execute(&id, edit(&target, ContextReplacement::Restore))
        .unwrap();
    assert!(
        restored
            .source_and_context()
            .unwrap()
            .1
            .last()
            .unwrap()
            .text_content()
            .starts_with("raw-11")
    );
    let checkout = nav
        .execute(
            &id,
            ArchiveNavigation::Checkout {
                target: Some(target),
            },
        )
        .unwrap();
    assert!(
        checkout
            .source_and_context()
            .unwrap()
            .1
            .last()
            .unwrap()
            .text_content()
            .starts_with("raw-11")
    );
    let mut cursor = None;
    let mut raw = Vec::new();
    loop {
        let page = fresh.archive_page(&id, 100, cursor.as_ref()).unwrap();
        raw.extend(page.entries);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        raw.iter().filter(|e| e.data.as_message().is_some()).count(),
        12
    );
    assert!(raw.iter().all(|e| {
        !serde_json::to_string(e)
            .unwrap()
            .contains("private index descriptor")
    }));
}
#[test]
fn cold_edit_targets_are_validated_even_outside_window_and_rejected_before_writes() {
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(storage.path()).unwrap();
    let mut s = writer.create(workspace.path()).unwrap();
    writer
        .append(
            &mut s,
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "a".into(),
                        name: "read".into(),
                        arguments: json!({"secret-argument":"original"}),
                    },
                }],
            },
        )
        .unwrap();
    let target = s.metadata().leaf_id.clone().unwrap();
    writer
        .append(
            &mut s,
            Message {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    call_id: "a".into(),
                    content: json!({"output":"done"}),
                    is_error: false,
                }],
            },
        )
        .unwrap();
    for _ in 0..8 {
        writer
            .append(&mut s, Message::text(MessageRole::User, "tail".repeat(300)))
            .unwrap();
    }
    let original = std::fs::read(s.path().unwrap()).unwrap();
    let repo = limited(storage.path(), 4096);
    let nav = SessionNavigator::new(&repo, workspace.path()).unwrap();
    assert!(
        nav.execute(
            &s.metadata().id,
            edit(
                &target,
                ContextReplacement::Blocks(vec![ContentBlock::Text {
                    text: "drop the call".into()
                }])
            )
        )
        .is_err()
    );
    assert_eq!(std::fs::read(s.path().unwrap()).unwrap(), original);
    // A valid cold edit is retained for a future larger view, without exposing cold descriptors.
    nav.execute(
        &s.metadata().id,
        edit(&target, ContextReplacement::Text("rationale".into())),
    )
    .unwrap();
    assert!(
        !repo
            .open_run(&s.metadata().id)
            .unwrap()
            .source_and_context()
            .unwrap()
            .1
            .iter()
            .any(|m| m.text_content().contains("rationale"))
    );
    let full = writer.open(&s.metadata().id).unwrap();
    let view = full.selected_branch().model_context().unwrap().messages;
    assert_eq!(
        view[0].tool_calls().next().unwrap().arguments["secret-argument"],
        "original"
    );
    assert_eq!(view[0].text_content(), "rationale");
}
#[test]
fn edit_payload_admission_is_bounded_and_plans_reject_changed_archives() {
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(storage.path()).unwrap();
    let s = seed(&writer, workspace.path());
    let target = s.entries().last().unwrap().id.clone();
    let repo = limited(storage.path(), 2200);
    let nav = SessionNavigator::new(&repo, workspace.path()).unwrap();
    let original = std::fs::read(s.path().unwrap()).unwrap();
    assert!(
        nav.prepare(
            &s.metadata().id,
            edit(&target, ContextReplacement::Text("x".repeat(1600)))
        )
        .is_err()
    );
    assert_eq!(std::fs::read(s.path().unwrap()).unwrap(), original);
    let plan = nav
        .prepare(&s.metadata().id, edit(&target, ContextReplacement::Omit))
        .unwrap();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(s.path().unwrap())
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    assert!(plan.commit().is_err());
    assert_eq!(
        std::fs::read(s.path().unwrap()).unwrap(),
        [original, b"\n".to_vec()].concat()
    );
}
