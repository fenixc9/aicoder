#[path = "../examples/support/transport_hooks.rs"]
mod support;
#[tokio::test]
async fn runtime_session_client_transport_and_request_view_hooks_compose_without_private_history() {
    for stream in [false, true] {
        let report = support::run(stream).await.unwrap();
        assert_eq!(report["transport_report_empty"], true);
        assert_eq!(report["projection_receipts"], 2);
    }
}
