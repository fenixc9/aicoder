#[path = "../examples/support/assistant_finalization.rs"]
mod support;
#[tokio::test]
async fn assistant_finalization_preserves_owned_effects_history_archive_and_export() {
    for allow in [true, false] {
        support::run(false, allow).await.unwrap();
    }
}
#[cfg(feature = "code-mode")]
#[tokio::test]
async fn code_mode_finalization_keeps_leaf_approval_and_metering() {
    for allow in [true, false] {
        support::run(true, allow).await.unwrap();
    }
}
