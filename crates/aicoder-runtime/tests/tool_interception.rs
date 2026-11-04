#[path = "../examples/support/tool_interception.rs"]
mod support;
#[tokio::test]
async fn native_interception_uses_real_owned_filesystem_plans_jsonl_and_output_events() {
    for allow in [true, false] {
        let result = support::run(false, allow).await.unwrap();
        assert_eq!(result["actual_mutation"], allow);
        assert!(result["interception_receipts"].as_u64().unwrap() >= 3);
    }
}
#[cfg(feature = "code-mode")]
#[tokio::test]
async fn composed_leaf_interception_shares_approval_receipts_and_filtered_model_results() {
    for allow in [true, false] {
        let result = support::run(true, allow).await.unwrap();
        assert_eq!(result["actual_mutation"], allow);
        assert_eq!(result["tool_messages"], 1);
        if allow {
            assert!(result["interception_receipts"].as_u64().unwrap() >= 7);
        }
    }
}
