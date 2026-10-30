use aicoder_core::{
    TurnExecutionContext,
    tools::{ExecutableTool, ToolContext},
};
use aicoder_runtime::{
    ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings, resources::ResourceSettings,
};
use std::{collections::BTreeMap, fs, os::unix::fs::symlink, sync::Arc};
#[tokio::test]
async fn references_reuse_bounded_pages_and_keep_immutable_content_and_stale_cursors() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = workspace.path().join("large.txt");
    fs::write(&path, "old".repeat(20000)).unwrap();
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Clear,
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::new()),
    )
    .unwrap();
    let reference = runtime
        .read_reference("large.txt", TurnExecutionContext::new())
        .await
        .unwrap();
    assert!(reference.encoded().len() <= 16 * 1024);
    assert_eq!(reference.page()["has_more"], true);
    assert!(reference.page()["next_cursor"].is_string());
    assert!(
        reference.page()["content"]
            .as_str()
            .unwrap()
            .starts_with("old")
    );
    fs::write(&path, "new").unwrap();
    assert!(
        reference.page()["content"]
            .as_str()
            .unwrap()
            .starts_with("old")
    );
    let context =
        ToolContext::new(workspace.path(), std::time::Duration::from_secs(2), 16384).unwrap();
    let error = aicoder_runtime::tools::ReadFileTool
        .execute(
            &context,
            serde_json::json!({"path":"large.txt","cursor":reference.page()["next_cursor"]}),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "stale_cursor");
    fs::write(outside.path().join("secret"), "outside").unwrap();
    symlink(
        outside.path().join("secret"),
        workspace.path().join("escape"),
    )
    .unwrap();
    assert!(
        runtime
            .read_reference("escape", TurnExecutionContext::new())
            .await
            .is_err()
    );
    fs::write(workspace.path().join("binary"), b"text\0data").unwrap();
    assert!(
        runtime
            .read_reference("binary", TurnExecutionContext::new())
            .await
            .is_err()
    );
    let cancelled = TurnExecutionContext::new();
    cancelled.cancel("stop");
    assert!(
        runtime
            .read_reference("large.txt", cancelled)
            .await
            .is_err()
    );
}
