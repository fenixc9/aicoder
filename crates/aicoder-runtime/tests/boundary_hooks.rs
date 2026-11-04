#[path = "../examples/support/boundary_hooks.rs"]
mod support;
#[tokio::test]
async fn native_boundary_context_continuation_jsonl_archive_and_export_share_real_runtime() {
    for allow in [true, false] {
        let report = support::run(false, allow).await.unwrap();
        assert_eq!(report["native_context_messages"], if allow { 3 } else { 0 });
    }
}
#[cfg(feature = "code-mode")]
#[tokio::test]
async fn code_mode_boundary_actions_preserve_leaf_approval_and_canonical_message_source() {
    for allow in [true, false] {
        let report = support::run(true, allow).await.unwrap();
        assert_eq!(report["original_user_count"], 1);
    }
}
