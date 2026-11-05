#[path = "../examples/support/input_hooks.rs"]
mod support;
#[tokio::test]
async fn raw_input_transforms_before_templates_and_preserves_owned_tool_approval_and_history() {
    for allow in [true, false] {
        support::run(false, allow, false).await.unwrap();
    }
}
#[tokio::test]
async fn handled_raw_input_never_expands_unknown_commands_or_fabricates_model_turns() {
    support::run(false, true, true).await.unwrap();
}
#[cfg(feature = "code-mode")]
#[tokio::test]
async fn code_mode_uses_the_same_input_and_leaf_approval_pipeline() {
    for allow in [true, false] {
        support::run(true, allow, false).await.unwrap();
    }
}
