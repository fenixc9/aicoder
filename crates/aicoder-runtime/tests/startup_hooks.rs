#[path = "../examples/support/startup_hooks.rs"]
mod support;
#[tokio::test]
async fn startup_uses_owned_tools_approvals_jsonl_archive_and_inert_export() {
    for allow in [true, false] {
        support::run(false, allow).await.unwrap();
    }
}
#[cfg(feature = "code-mode")]
#[tokio::test]
async fn startup_preserves_code_mode_only_roots_and_leaf_approvals() {
    for allow in [true, false] {
        support::run(true, allow).await.unwrap();
    }
}
