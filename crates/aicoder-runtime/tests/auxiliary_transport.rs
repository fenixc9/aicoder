#![cfg(feature = "auxiliary-models")]
#[path = "../examples/support/auxiliary_transport.rs"]
mod support;
#[tokio::test]
async fn runtime_binding_reaches_native_clients_across_reload_while_authority_and_usage_stay_explicit()
 {
    for errors in [false, true] {
        let report = support::run(false, true, errors).await.unwrap();
        assert_eq!(report["native_tasks"], 2);
        assert_eq!(report["usage"], 37);
    }
}
#[tokio::test]
async fn denied_native_approval_never_enters_auxiliary_transport() {
    let report = support::run(false, false, false).await.unwrap();
    assert_eq!(report["native_tasks"], 0);
    assert_eq!(report["loopback_requests"], 2);
}
#[cfg(feature = "code-mode")]
#[tokio::test]
async fn code_mode_native_helpers_share_binding_and_parent_usage_after_reload() {
    for allow in [false, true] {
        let report = support::run(true, allow, false).await.unwrap();
        assert_eq!(report["native_tasks"], if allow { 2 } else { 0 });
    }
}
