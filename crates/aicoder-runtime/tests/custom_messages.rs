#[path = "../examples/support/custom_messages.rs"]
mod support;
#[tokio::test]
async fn idle_custom_runs_preserve_sources_owned_approvals_jsonl_archive_and_export() {
    for allow in [true, false] {
        support::run(false, allow, false).await.unwrap();
    }
}
#[tokio::test]
async fn idle_custom_storage_creates_no_model_turn_or_user_message() {
    support::run(false, true, true).await.unwrap();
}
#[cfg(feature = "code-mode")]
#[tokio::test]
async fn custom_code_mode_runs_preserve_only_roots_and_leaf_approval() {
    for allow in [true, false] {
        support::run(true, allow, false).await.unwrap();
    }
}
