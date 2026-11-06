#[path = "../examples/support/custom_delivery.rs"]
mod support;
#[tokio::test]
async fn custom_control_delivery_uses_real_owned_tools_journal_windows_and_inert_exports() {
    for delivery in [
        aicoder_core::CustomDelivery::Steer,
        aicoder_core::CustomDelivery::FollowUp,
        aicoder_core::CustomDelivery::TurnEnd,
        aicoder_core::CustomDelivery::NextTurn,
    ] {
        support::run(false, true, delivery).await.unwrap();
    }
    support::run(false, false, aicoder_core::CustomDelivery::TurnEnd)
        .await
        .unwrap();
}
#[cfg(feature = "code-mode")]
#[tokio::test]
async fn code_mode_custom_delivery_keeps_only_roots_leaf_approval_and_parent_metering() {
    for delivery in [
        aicoder_core::CustomDelivery::Steer,
        aicoder_core::CustomDelivery::FollowUp,
        aicoder_core::CustomDelivery::TurnEnd,
        aicoder_core::CustomDelivery::NextTurn,
    ] {
        support::run(true, true, delivery).await.unwrap();
    }
    support::run(true, false, aicoder_core::CustomDelivery::TurnEnd)
        .await
        .unwrap();
}
