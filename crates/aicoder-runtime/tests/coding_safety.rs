#[path = "../examples/support/coding_safety.rs"]
mod coding_safety;
use aicoder_core::{extensions::*, model::*, tools::*, *};
use aicoder_runtime::tools::{BashTool, EditFileTool, WriteFileTool};
use async_trait::async_trait;
use coding_safety::CodingSafety;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
fn context(root: &Path) -> ToolContext {
    ToolContext::new(root, Duration::from_secs(5), 16384).unwrap()
}
fn safety(root: &Path) -> Arc<CodingSafety> {
    std::fs::create_dir(root.join("protected")).unwrap();
    CodingSafety::new(root, &["protected"]).unwrap()
}
async fn mode(safety: Arc<CodingSafety>, workspace: &Path, enabled: bool) {
    let mut extension = Extension::new("policy", "1").unwrap();
    extension.command(safety, Duration::from_secs(5)).unwrap();
    let mut registry = ExtensionRegistry::new();
    registry.install(extension).unwrap();
    registry
        .finish()
        .execute_command(
            "plan",
            CommandRequest {
                context: CommandContext {
                    workspace,
                    session: None,
                },
                arguments: &json!({"enabled":enabled}),
            },
            &TurnExecutionContext::new(),
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn policies_check_lexical_and_captured_targets_without_changing_the_prepared_preview() {
    let root = tempfile::tempdir().unwrap();
    let safety = safety(root.path());
    let context = context(root.path());
    std::fs::write(root.path().join("note.txt"), "before").unwrap();
    std::fs::write(root.path().join("protected/key"), "KEEP").unwrap();
    std::os::unix::fs::symlink("protected/key", root.path().join("alias")).unwrap();
    let tool = safety.wrap(Arc::new(WriteFileTool));
    for path in ["protected/key", "./allowed/../protected/key", "alias"] {
        assert_eq!(
            tool.prepare(&context, json!({"path":path,"content":"bad"}))
                .await
                .err()
                .unwrap()
                .code,
            "protected_path"
        );
    }
    let direct = WriteFileTool
        .prepare(&context, json!({"path":"note.txt","content":"after"}))
        .await
        .unwrap()
        .unwrap();
    let wrapped = tool
        .prepare(&context, json!({"path":"note.txt","content":"after"}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wrapped.preview().summary, direct.preview().summary);
    assert_eq!(wrapped.preview().body, direct.preview().body);
    assert!(
        matches!(wrapped.effects(),PreparedToolEffects::FileMutation {paths} if paths==[root.path().join("note.txt")])
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("note.txt")).unwrap(),
        "before"
    );
    wrapped.execute(&context).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("note.txt")).unwrap(),
        "after"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("protected/key")).unwrap(),
        "KEEP"
    );
    assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".aicoder-")
    }));
}
#[tokio::test]
async fn edit_uses_the_same_captured_effects_and_opaque_commands_fail_before_execution() {
    let root = tempfile::tempdir().unwrap();
    let safety = safety(root.path());
    let context = context(root.path());
    std::fs::write(root.path().join("protected/key"), "before").unwrap();
    assert_eq!(
        safety
            .wrap(Arc::new(EditFileTool))
            .prepare(
                &context,
                json!({"path":"protected/key","old_text":"before","new_text":"after"})
            )
            .await
            .err()
            .unwrap()
            .code,
        "protected_path"
    );
    assert_eq!(
        safety
            .wrap(Arc::new(BashTool))
            .prepare(&context, json!({"command":"touch should_not_exist"}))
            .await
            .err()
            .unwrap()
            .code,
        "opaque_effects"
    );
    assert!(!root.path().join("should_not_exist").exists());
}
#[tokio::test]
async fn an_epoch_change_invalidates_a_prepared_write_even_if_planning_is_disabled_again() {
    let root = tempfile::tempdir().unwrap();
    let safety = safety(root.path());
    let context = context(root.path());
    std::fs::write(root.path().join("note.txt"), "before").unwrap();
    let tool = safety.wrap(Arc::new(WriteFileTool));
    let plan = tool
        .prepare(&context, json!({"path":"note.txt","content":"after"}))
        .await
        .unwrap()
        .unwrap();
    mode(safety.clone(), root.path(), true).await;
    assert!(safety.planning().await);
    mode(safety.clone(), root.path(), false).await;
    assert_eq!(
        plan.execute(&context).await.unwrap_err().code,
        "policy_changed"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("note.txt")).unwrap(),
        "before"
    );
}
#[tokio::test]
async fn changed_aliases_and_changed_policy_routes_cannot_publish_an_approved_plan() {
    let root = tempfile::tempdir().unwrap();
    let safety = safety(root.path());
    let context = context(root.path());
    std::fs::write(root.path().join("note.txt"), "before").unwrap();
    std::fs::write(root.path().join("protected/key"), "KEEP").unwrap();
    std::os::unix::fs::symlink("note.txt", root.path().join("alias")).unwrap();
    let tool = safety.wrap(Arc::new(WriteFileTool));
    let plan = tool
        .prepare(&context, json!({"path":"alias","content":"bad"}))
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_file(root.path().join("alias")).unwrap();
    std::os::unix::fs::symlink("protected/key", root.path().join("alias")).unwrap();
    assert_eq!(
        plan.execute(&context).await.unwrap_err().code,
        "edit_conflict"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("protected/key")).unwrap(),
        "KEEP"
    );
    let plan = tool
        .prepare(&context, json!({"path":"note.txt","content":"after"}))
        .await
        .unwrap()
        .unwrap();
    std::fs::rename(root.path().join("protected"), root.path().join("moved")).unwrap();
    std::os::unix::fs::symlink("moved", root.path().join("protected")).unwrap();
    assert_eq!(
        plan.execute(&context).await.unwrap_err().code,
        "policy_stale"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("note.txt")).unwrap(),
        "before"
    );
}
struct PausedTool {
    path: PathBuf,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    committed: Arc<AtomicBool>,
}
struct PausedPlan {
    path: PathBuf,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    committed: Arc<AtomicBool>,
}
#[async_trait]
impl ExecutableTool for PausedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "paused".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn prepare(
        &self,
        _: &ToolContext,
        _: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        Ok(Some(Box::new(PausedPlan {
            path: self.path.clone(),
            started: self.started.clone(),
            release: self.release.clone(),
            committed: self.committed.clone(),
        })))
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        unreachable!()
    }
}
#[async_trait]
impl PreparedTool for PausedPlan {
    fn effects(&self) -> PreparedToolEffects<'_> {
        PreparedToolEffects::FileMutation {
            paths: std::slice::from_ref(&self.path),
        }
    }
    fn preview(&self) -> ToolPreview {
        ToolPreview {
            summary: "Paused operation".into(),
            body: String::new(),
            format: "text".into(),
            truncated: false,
        }
    }
    async fn execute(self: Box<Self>, _: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        self.started.notify_one();
        self.release.notified().await;
        self.committed.store(true, Ordering::SeqCst);
        Ok(ToolSuccess::new(json!({"committed":true})))
    }
}
#[tokio::test]
async fn enabling_planning_waits_for_an_already_admitted_mutation() {
    let root = tempfile::tempdir().unwrap();
    let safety = safety(root.path());
    let context = context(root.path());
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let committed = Arc::new(AtomicBool::new(false));
    let tool = safety.wrap(Arc::new(PausedTool {
        path: root.path().join("note"),
        started: started.clone(),
        release: release.clone(),
        committed: committed.clone(),
    }));
    let plan = tool.prepare(&context, json!({})).await.unwrap().unwrap();
    let writer = tokio::spawn(async move { plan.execute(&context).await });
    started.notified().await;
    let policy = safety.clone();
    let workspace = root.path().to_path_buf();
    let toggle = tokio::spawn(async move { mode(policy, &workspace, true).await });
    tokio::task::yield_now().await;
    assert!(!toggle.is_finished());
    assert!(!committed.load(Ordering::SeqCst));
    release.notify_one();
    writer.await.unwrap().unwrap();
    toggle.await.unwrap();
    assert!(committed.load(Ordering::SeqCst));
    assert!(safety.planning().await);
}
