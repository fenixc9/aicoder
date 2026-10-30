use aicoder_core::{
    TurnExecutionContext,
    model::{ContentBlock, Message, MessageRole, ModelToolCall},
    session::*,
};
use aicoder_runtime::{
    exports::*,
    session::{JsonlSessionRepository, SessionLoadLimits},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};
fn user(text: impl Into<String>) -> Message {
    Message::text(MessageRole::User, text)
}
fn prepared(
    repo: &JsonlSessionRepository,
    workspace: &Path,
    id: &str,
    format: ExportFormat,
) -> PreparedExport {
    prepare(
        repo,
        workspace,
        id,
        format,
        ExportLimits::default(),
        &TurnExecutionContext::new(),
    )
    .unwrap()
}
#[test]
fn json_roundtrips_canonical_branches_and_keeps_interruption_without_repairs() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append_all(
        &mut session,
        [
            user("old branch"),
            Message::text(MessageRole::Assistant, "old answer"),
        ],
    )
    .unwrap();
    SessionNavigator::new(&repo, workspace.path())
        .unwrap()
        .execute(
            &session.metadata().id,
            ArchiveNavigation::Branch {
                source: BranchSource::Root,
                name: Some("new branch".into()),
            },
        )
        .unwrap();
    session = repo.open(&session.metadata().id).unwrap();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::TurnStart {
                turn_id: "unfinished".into(),
            },
            SessionEntryData::Message {
                message: user("new branch <script>unsafe()</script>"),
            },
            SessionEntryData::Message {
                message: Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "pending".into(),
                            name: "bash".into(),
                            arguments: json!({"command":"never run"}),
                        },
                    }],
                },
            },
        ],
    )
    .unwrap();
    let original = fs::read(session.path().unwrap()).unwrap();
    let output = prepared(
        &repo,
        workspace.path(),
        &session.metadata().id,
        ExportFormat::Json,
    );
    let value: Value = serde_json::from_slice(output.bytes()).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["manifest"]["unfinished_turn"], "unfinished");
    let raw = value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| serde_json::from_value::<SessionEntry>(v["entry"].clone()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::to_value(&raw).unwrap(),
        serde_json::to_value(session.entries()).unwrap()
    );
    assert!(
        value["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["selected_branch"] == false)
    );
    let rebuilt = Session::from_entries(
        serde_json::from_value(value["manifest"]["metadata"].clone()).unwrap(),
        raw,
    )
    .unwrap();
    assert_eq!(rebuilt.unfinished_turn(), Some("unfinished"));
    assert!(rebuilt.run_token().is_none());
    assert!(
        !output
            .bytes()
            .windows(21)
            .any(|w| w == b"execution_interrupted")
    );
    assert_eq!(fs::read(session.path().unwrap()).unwrap(), original);
}
#[test]
fn cold_records_export_without_a_full_snapshot_or_materialized_model_window() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    for i in 0..20 {
        repo.append(&mut session, user(format!("cold-{i} {}", "x".repeat(8000))))
            .unwrap();
    }
    let reader = JsonlSessionRepository::with_limits(
        root.path(),
        SessionLoadLimits {
            max_snapshot_bytes: 1024,
            max_window_bytes: 1000,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(reader.open(&session.metadata().id).is_err());
    assert!(reader.open_run(&session.metadata().id).is_err());
    let output = prepared(
        &reader,
        workspace.path(),
        &session.metadata().id,
        ExportFormat::Json,
    );
    let value: Value = serde_json::from_slice(output.bytes()).unwrap();
    assert_eq!(value["entries"].as_array().unwrap().len(), 20);
    assert!(value.to_string().contains("cold-0"));
    assert!(!value.to_string().contains("private index descriptor"));
}
#[test]
fn html_escapes_all_untrusted_payloads_and_never_links_or_embeds_them() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append(&mut session,user("</pre><script>window.injected=true</script><img src='https://remote.invalid/x'>&\"quote")).unwrap();
    repo.append(&mut session, user("界".repeat(4000))).unwrap();
    let output = prepared(
        &repo,
        workspace.path(),
        &session.metadata().id,
        ExportFormat::Html,
    );
    let text = std::str::from_utf8(output.bytes()).unwrap();
    assert!(!text.contains("<script>"));
    assert!(!text.contains("<img "));
    assert!(text.contains("&lt;script&gt;"));
    assert!(text.contains("script-src 'none'"));
    assert!(text.contains("connect-src 'none'"));
    assert!(text.contains("Display preview clipped"));
    assert!(
        text.contains(&"界".repeat(4000)),
        "complete canonical data must survive preview clipping"
    );
    assert!(text.contains("all branches"));
    assert!(text.contains("https://remote.invalid/x"));
    assert!(!text.contains("href="));
}
#[test]
fn budgets_scope_busy_source_and_cancellation_fail_without_changing_history() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append_all(&mut session, [user("one"), user("two & <three>")])
        .unwrap();
    let original = fs::read(session.path().unwrap()).unwrap();
    for limits in [
        ExportLimits {
            max_source_bytes: 1,
            ..Default::default()
        },
        ExportLimits {
            max_entries: 1,
            ..Default::default()
        },
        ExportLimits {
            max_output_bytes: 20,
            ..Default::default()
        },
    ] {
        assert!(
            prepare(
                &repo,
                workspace.path(),
                &session.metadata().id,
                ExportFormat::Html,
                limits,
                &TurnExecutionContext::new()
            )
            .is_err()
        );
    }
    assert!(
        prepare(
            &repo,
            other.path(),
            &session.metadata().id,
            ExportFormat::Json,
            ExportLimits::default(),
            &TurnExecutionContext::new()
        )
        .is_err()
    );
    let context = TurnExecutionContext::new();
    context.cancel("test");
    assert!(
        prepare(
            &repo,
            workspace.path(),
            &session.metadata().id,
            ExportFormat::Json,
            ExportLimits::default(),
            &context
        )
        .is_err()
    );
    let lease = repo.acquire_run(&mut session).unwrap();
    assert!(
        prepare(
            &repo,
            workspace.path(),
            &session.metadata().id,
            ExportFormat::Json,
            ExportLimits::default(),
            &TurnExecutionContext::new()
        )
        .is_err()
    );
    drop(lease);
    let metadata = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            root.path()
                .join(".locks")
                .join(format!("{}.lock", session.metadata().id)),
        )
        .unwrap();
    fs2::FileExt::lock_exclusive(&metadata).unwrap();
    assert!(
        prepare(
            &repo,
            workspace.path(),
            &session.metadata().id,
            ExportFormat::Json,
            ExportLimits::default(),
            &TurnExecutionContext::new()
        )
        .err()
        .unwrap()
        .to_string()
        .contains("metadata is busy")
    );
    fs2::FileExt::unlock(&metadata).unwrap();
    assert_eq!(fs::read(session.path().unwrap()).unwrap(), original);
}
#[test]
fn torn_tail_is_declared_and_never_repaired_by_export() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append(&mut session, user("retained")).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(session.path().unwrap())
        .unwrap()
        .write_all(b"{\"type\":")
        .unwrap();
    let original = fs::read(session.path().unwrap()).unwrap();
    let output = prepared(
        &repo,
        workspace.path(),
        &session.metadata().id,
        ExportFormat::Json,
    );
    let value: Value = serde_json::from_slice(output.bytes()).unwrap();
    assert_eq!(value["manifest"]["ignored_tail_bytes"], 8);
    assert_eq!(value["manifest"]["source_tail_complete"], false);
    assert_eq!(value["canonical_entries_complete"], true);
    assert_eq!(value["entries"].as_array().unwrap().len(), 1);
    assert!(!output.receipt().source_tail_complete);
    assert_eq!(fs::read(session.path().unwrap()).unwrap(), original);
}
#[test]
fn legacy_compatibility_exports_without_implicit_migration() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let path = root.path().join("legacy.jsonl");
    let original = format!(
        "{}\n{}\n",
        json!({"type":"session","version":2,"id":id,"created_at":1,"cwd":workspace.path()}),
        json!({"type":"message","id":"legacy-user","created_at":2,"message":user("old") })
    );
    fs::write(&path, &original).unwrap();
    let repo = JsonlSessionRepository::new(root.path()).unwrap();
    let output = prepared(&repo, workspace.path(), &id, ExportFormat::Json);
    let value: Value = serde_json::from_slice(output.bytes()).unwrap();
    assert_eq!(value["manifest"]["metadata"]["version"], 2);
    assert_eq!(value["entries"][0]["entry"]["id"], "legacy-user");
    assert_eq!(fs::read_to_string(path).unwrap(), original);
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|s| s == "bak"))
            .count(),
        0
    );
}
#[tokio::test]
async fn owned_preparation_publishes_exact_bytes_only_when_explicitly_called() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repo.create(workspace.path()).unwrap();
    repo.append(&mut session, user("snapshot")).unwrap();
    let export = prepared(
        &repo,
        workspace.path(),
        &session.metadata().id,
        ExportFormat::Html,
    );
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
    repo.append(&mut session, user("later private data"))
        .unwrap();
    let publisher =
        DirectoryPublisher::new(destination.path(), Some("https://example.invalid/share")).unwrap();
    let receipt = publisher
        .publish(&export, TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(fs::read(&receipt.path).unwrap(), export.bytes());
    assert!(
        receipt
            .url
            .unwrap()
            .starts_with("https://example.invalid/share/")
    );
    assert!(
        !std::str::from_utf8(export.bytes())
            .unwrap()
            .contains("later private data")
    );
    assert_eq!(
        fs::metadata(&receipt.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let existing = workspace.path().join("existing.json");
    fs::write(&existing, b"keep").unwrap();
    let error = export
        .write_new(&existing, &TurnExecutionContext::new())
        .unwrap_err();
    assert!(!error.downcast_ref::<CommitFailure>().unwrap().visible);
    assert_eq!(fs::read(&existing).unwrap(), b"keep");
    let linked = workspace.path().join("linked.json");
    symlink(&existing, &linked).unwrap();
    assert!(
        export
            .write_new(&linked, &TurnExecutionContext::new())
            .is_err()
    );
    assert_eq!(fs::read(&existing).unwrap(), b"keep");
    use std::os::unix::ffi::OsStringExt;
    let non_utf8 = workspace
        .path()
        .join(std::ffi::OsString::from_vec(vec![0xff]));
    fs::create_dir(&non_utf8).unwrap();
    let alias = workspace.path().join("unicode-alias");
    symlink(&non_utf8, &alias).unwrap();
    assert!(
        export
            .write_new(&alias.join("never.json"), &TurnExecutionContext::new())
            .is_err()
    );
    assert_eq!(fs::read_dir(&non_utf8).unwrap().count(), 0);
    let cancelled = TurnExecutionContext::new();
    cancelled.cancel("test");
    assert!(publisher.publish(&export, cancelled).await.is_err());
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 1);
    assert!(fs::read_dir(workspace.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")
    }));
    assert!(
        DirectoryPublisher::new(
            destination.path(),
            Some("https://user:secret@example.invalid/")
        )
        .is_err()
    );
}
