#[path = "../examples/support/request_views.rs"]
mod support;
#[tokio::test]
async fn compatible_http_and_jsonl_preserve_transformed_views_options_tools_and_usage() {
    for stream in [false, true] {
        let report = support::run(stream).await.unwrap();
        assert_eq!(report["projection_receipts"], 2);
        assert_eq!(report["usage"], 11);
    }
}
