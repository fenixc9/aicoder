use aicoder_core::{
    model::ModelToolCall,
    tools::{
        AllowAllApproval, ApprovalHandler, DispatcherConfig, ExecutableTool,
        MAX_TOOL_PREVIEW_BYTES, ToolContext, ToolDispatcher, ToolInvocation,
    },
};
use aicoder_runtime::tools::{
    EditFileTool, ReadFileTool, WriteFileTool, default_registry, display_preview,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
fn context(root: &Path) -> ToolContext {
    ToolContext::new(root, Duration::from_secs(5), 4096).unwrap()
}
struct Review<F>(F);
#[async_trait]
impl<F: Fn(&ToolInvocation) -> bool + Send + Sync> ApprovalHandler for Review<F> {
    async fn approve(&self, invocation: &ToolInvocation) -> anyhow::Result<bool> {
        Ok((self.0)(invocation))
    }
}
fn dispatcher(root: &Path, approval: impl ApprovalHandler + 'static) -> ToolDispatcher {
    ToolDispatcher::new(
        Arc::new(default_registry().unwrap()),
        root,
        Arc::new(approval),
        DispatcherConfig::default(),
    )
    .unwrap()
}
async fn dispatch(dispatcher: &ToolDispatcher, name: &str, args: Value) -> Value {
    let messages = dispatcher
        .dispatch(&[ModelToolCall {
            id: "call".into(),
            name: name.into(),
            arguments: args,
        }])
        .await
        .unwrap();
    messages[0].tool_result().unwrap().1.clone()
}
fn entries(root: &Path) -> Vec<String> {
    let mut entries = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

#[tokio::test]
async fn preparation_is_read_only_and_commit_uses_the_exact_reviewed_change() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), "alpha\nβeta\n").unwrap();
    let ctx = context(root.path());
    let plan = EditFileTool
        .prepare(
            &ctx,
            json!({"path":"file","edits":[{"old_text":"alpha","new_text":"ALPHA"}]}),
        )
        .await
        .unwrap()
        .unwrap();
    let preview = plan.preview();
    assert!(preview.body.contains("-alpha"));
    assert!(preview.body.contains("+ALPHA"));
    assert!(!preview.truncated);
    assert_eq!(
        fs::read_to_string(root.path().join("file")).unwrap(),
        "alpha\nβeta\n"
    );
    assert_eq!(entries(root.path()), ["file"]);
    let result = plan.execute(&ctx).await.unwrap();
    assert_eq!(result.output["edits_applied"], 1);
    assert_eq!(
        fs::read_to_string(root.path().join("file")).unwrap(),
        "ALPHA\nβeta\n"
    );
    let read = ReadFileTool
        .execute(&ctx, json!({"path":"file"}))
        .await
        .unwrap();
    assert_eq!(result.output["revision"], read.output["revision"]);
    assert_eq!(entries(root.path()), ["file"]);
}
#[tokio::test]
async fn changes_during_approval_are_conflicts_and_the_preview_is_present_in_events() {
    use aicoder_core::{AgentRawEvent, AgentRawEventEnvelope, events::AgentEventEmitter};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    fs::write(&path, "before\n").unwrap();
    let target = path.clone();
    let approval = Review(move |invocation: &ToolInvocation| {
        let preview = invocation.preview.as_ref().unwrap();
        assert!(preview.body.contains("-before"));
        assert!(preview.body.contains("+after"));
        fs::write(&target, "another editor\n").unwrap();
        true
    });
    let dispatcher = dispatcher(root.path(), approval);
    let frames = Arc::new(Mutex::new(Vec::new()));
    let capture = frames.clone();
    let events = AgentEventEmitter::new(Arc::new(move |frame: &AgentRawEventEnvelope| {
        capture.lock().unwrap().push(frame.clone())
    }));
    let messages=dispatcher.dispatch_with_events(&[ModelToolCall{id:"call".into(),name:"edit_file".into(),arguments:json!({"path":"file","edits":[{"old_text":"before","new_text":"after"}]})}],&events).await.unwrap();
    events.shutdown().await;
    assert_eq!(
        messages[0].tool_result().unwrap().1["error"]["code"],
        "edit_conflict"
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "another editor\n");
    assert!(frames.lock().unwrap().iter().any(|f|matches!(&f.event,AgentRawEvent::ToolApprovalRequested{preview:Some(p),..} if p.body.contains("+after"))));
    assert_eq!(entries(root.path()), ["file"]);
}
#[tokio::test]
async fn an_external_creator_during_approval_is_never_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("new");
    let target = path.clone();
    let dispatcher = dispatcher(
        root.path(),
        Review(move |invocation: &ToolInvocation| {
            assert!(
                invocation
                    .preview
                    .as_ref()
                    .unwrap()
                    .body
                    .contains("/dev/null")
            );
            fs::write(&target, "external").unwrap();
            true
        }),
    );
    let result = dispatch(
        &dispatcher,
        "write_file",
        json!({"path":"new","content":"agent","expected_revision":"missing"}),
    )
    .await;
    assert_eq!(result["error"]["code"], "edit_conflict");
    assert_eq!(fs::read_to_string(path).unwrap(), "external");
    assert_eq!(entries(root.path()), ["new"]);
}
#[tokio::test]
async fn concurrent_prepared_replacements_and_creates_have_one_winner() {
    for exists in [false, true] {
        let root = tempfile::tempdir().unwrap();
        if exists {
            fs::write(root.path().join("file"), "before").unwrap();
        }
        let ctx = context(root.path());
        let first = WriteFileTool
            .prepare(&ctx, json!({"path":"file","content":"first"}))
            .await
            .unwrap()
            .unwrap();
        let second = WriteFileTool
            .prepare(&ctx, json!({"path":"file","content":"second"}))
            .await
            .unwrap()
            .unwrap();
        let (a, b) = tokio::join!(first.execute(&ctx), second.execute(&ctx));
        assert_ne!(a.is_ok(), b.is_ok());
        let error = if let Err(error) = a {
            error
        } else {
            b.unwrap_err()
        };
        assert_eq!(error.code, "edit_conflict");
        assert!(matches!(
            fs::read_to_string(root.path().join("file"))
                .unwrap()
                .as_str(),
            "first" | "second"
        ));
        assert_eq!(entries(root.path()), ["file"]);
    }
}
#[tokio::test]
async fn stale_read_revisions_prevent_writes_before_requesting_approval() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    fs::write(&path, "before").unwrap();
    let ctx = context(root.path());
    let revision = ReadFileTool
        .execute(&ctx, json!({"path":"file"}))
        .await
        .unwrap()
        .output["revision"]
        .clone();
    fs::write(&path, "external").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    let dispatcher = dispatcher(
        root.path(),
        Review(move |_: &ToolInvocation| {
            observed.fetch_add(1, Ordering::SeqCst);
            true
        }),
    );
    let result = dispatch(
        &dispatcher,
        "write_file",
        json!({"path":"file","content":"after","expected_revision":revision}),
    )
    .await;
    assert_eq!(result["error"]["code"], "edit_conflict");
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(fs::read_to_string(path).unwrap(), "external");
}
#[tokio::test]
async fn invalid_edits_and_denials_never_stage_or_modify_files() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), "before").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    let dispatcher = dispatcher(
        root.path(),
        Review(move |_: &ToolInvocation| {
            observed.fetch_add(1, Ordering::SeqCst);
            false
        }),
    );
    let invalid = dispatch(
        &dispatcher,
        "edit_file",
        json!({"path":"file","edits":[{"old_text":"missing","new_text":"after"}]}),
    )
    .await;
    assert_eq!(invalid["error"]["code"], "match_not_found");
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let denied = dispatch(
        &dispatcher,
        "write_file",
        json!({"path":"new","content":"after"}),
    )
    .await;
    assert_eq!(denied["error"]["code"], "approval_denied");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(entries(root.path()), ["file"]);
}
#[cfg(unix)]
#[tokio::test]
async fn parent_replacement_and_alias_retarget_cannot_redirect_a_reviewed_write() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("directory")).unwrap();
    fs::write(root.path().join("directory/file"), "before").unwrap();
    fs::write(outside.path().join("file"), "outside").unwrap();
    symlink(root.path().join("directory"), root.path().join("alias")).unwrap();
    let alias = root.path().join("alias");
    let redirected = outside.path().to_path_buf();
    let first = dispatcher(
        root.path(),
        Review(move |_: &ToolInvocation| {
            fs::remove_file(&alias).unwrap();
            symlink(&redirected, &alias).unwrap();
            true
        }),
    );
    let result = dispatch(
        &first,
        "write_file",
        json!({"path":"alias/file","content":"agent"}),
    )
    .await;
    assert_eq!(result["error"]["code"], "edit_conflict");
    assert_eq!(
        fs::read_to_string(outside.path().join("file")).unwrap(),
        "outside"
    );
    let dir = root.path().join("directory");
    let moved = root.path().join("moved");
    let second = dispatcher(
        root.path(),
        Review(move |_: &ToolInvocation| {
            fs::rename(&dir, &moved).unwrap();
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("file"), "replacement directory").unwrap();
            true
        }),
    );
    let result = dispatch(
        &second,
        "write_file",
        json!({"path":"directory/file","content":"agent"}),
    )
    .await;
    assert_eq!(result["error"]["code"], "edit_conflict");
    assert_eq!(
        fs::read_to_string(root.path().join("moved/file")).unwrap(),
        "before"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("directory/file")).unwrap(),
        "replacement directory"
    );
}
#[cfg(unix)]
#[tokio::test]
async fn dangling_symlinks_special_files_and_mode_changes_are_not_blindly_replaced() {
    use std::{
        ffi::CString,
        os::unix::{
            ffi::OsStrExt,
            fs::{PermissionsExt, symlink},
        },
    };
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let ctx = context(root.path());
    symlink(outside.path().join("missing"), root.path().join("dangling")).unwrap();
    assert!(
        WriteFileTool
            .execute(&ctx, json!({"path":"dangling","content":"agent"}))
            .await
            .is_err()
    );
    assert!(!outside.path().join("missing").exists());
    let pipe = root.path().join("pipe");
    let name = CString::new(pipe.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        WriteFileTool.execute(&ctx, json!({"path":"pipe","content":"agent"})),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code, "not_a_file");
    fs::write(root.path().join("file"), "before").unwrap();
    fs::set_permissions(root.path().join("file"), fs::Permissions::from_mode(0o755)).unwrap();
    let plan = WriteFileTool
        .prepare(&ctx, json!({"path":"file","content":"after"}))
        .await
        .unwrap()
        .unwrap();
    plan.execute(&ctx).await.unwrap();
    assert_eq!(
        fs::metadata(root.path().join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let plan = WriteFileTool
        .prepare(&ctx, json!({"path":"file","content":"later"}))
        .await
        .unwrap()
        .unwrap();
    fs::set_permissions(root.path().join("file"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(plan.execute(&ctx).await.unwrap_err().code, "edit_conflict");
    assert_eq!(
        fs::read_to_string(root.path().join("file")).unwrap(),
        "after"
    );
}
struct WaitingApproval(Arc<tokio::sync::Notify>);
#[async_trait]
impl ApprovalHandler for WaitingApproval {
    async fn approve(&self, _: &ToolInvocation) -> anyhow::Result<bool> {
        self.0.notify_one();
        std::future::pending().await
    }
}
#[tokio::test]
async fn dropping_a_pending_approval_has_no_mutation_or_staging_side_effects() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), "before").unwrap();
    let waiting = Arc::new(tokio::sync::Notify::new());
    let dispatcher = Arc::new(dispatcher(root.path(), WaitingApproval(waiting.clone())));
    let worker = tokio::spawn(async move {
        dispatch(
            &dispatcher,
            "write_file",
            json!({"path":"file","content":"after"}),
        )
        .await
    });
    waiting.notified().await;
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    assert_eq!(
        fs::read_to_string(root.path().join("file")).unwrap(),
        "before"
    );
    assert_eq!(entries(root.path()), ["file"]);
}
#[tokio::test]
async fn a_staging_deadline_cannot_publish_or_leave_a_temporary_path() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), "before").unwrap();
    let ctx = context(root.path());
    let plan = WriteFileTool
        .prepare(
            &ctx,
            json!({"path":"file","content":"x".repeat(4*1024*1024)}),
        )
        .await
        .unwrap()
        .unwrap();
    let short = ToolContext::new(root.path(), Duration::ZERO, 4096).unwrap();
    assert_eq!(plan.execute(&short).await.unwrap_err().code, "timeout");
    assert_eq!(
        fs::read_to_string(root.path().join("file")).unwrap(),
        "before"
    );
    assert_eq!(entries(root.path()), ["file"]);
}
#[tokio::test]
async fn previews_and_input_sizes_are_bounded_and_terminal_controls_are_escaped() {
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path());
    let plan = WriteFileTool
        .prepare(
            &ctx,
            json!({"path":"file","content":"\u{1b}[31m\u{202e}\n".repeat(4000)}),
        )
        .await
        .unwrap()
        .unwrap();
    let preview = plan.preview();
    assert!(preview.truncated);
    assert!(serde_json::to_vec(&preview).unwrap().len() <= MAX_TOOL_PREVIEW_BYTES);
    let display = display_preview(&preview);
    assert!(!display.contains('\u{1b}'));
    assert!(!display.contains('\u{202e}'));
    drop(plan);
    assert!(entries(root.path()).is_empty());
    assert_eq!(
        WriteFileTool
            .execute(
                &ctx,
                json!({"path":"file","content":"x".repeat(4*1024*1024+1)})
            )
            .await
            .unwrap_err()
            .code,
        "file_too_large"
    );
    let tiny = ToolContext::new(root.path(), Duration::from_secs(5), 5).unwrap();
    assert_eq!(
        WriteFileTool
            .execute(&tiny, json!({"path":"tiny","content":"new"}))
            .await
            .unwrap_err()
            .code,
        "output_limit_too_small"
    );
    assert!(!root.path().join("tiny").exists());
    let dispatcher = dispatcher(root.path(), AllowAllApproval);
    assert_eq!(
        dispatch(
            &dispatcher,
            "write_file",
            json!({"path":"file","content":"new"})
        )
        .await["ok"],
        true
    );
}
