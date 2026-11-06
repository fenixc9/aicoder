//! Busy custom delivery and next-turn claims through actual runtime tools and JSONL, offline.
#[path = "support/custom_delivery.rs"]
mod support;
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut mode = false;
    let mut allow = true;
    let mut delivery = aicoder_core::CustomDelivery::TurnEnd;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--code-mode" => mode = true,
            "--deny" => allow = false,
            "--delivery" => {
                i += 1;
                delivery = match args.get(i).map(String::as_str) {
                    Some("steer") => aicoder_core::CustomDelivery::Steer,
                    Some("follow_up") => aicoder_core::CustomDelivery::FollowUp,
                    Some("turn_end") => aicoder_core::CustomDelivery::TurnEnd,
                    Some("next_turn") => aicoder_core::CustomDelivery::NextTurn,
                    _ => anyhow::bail!("delivery: steer, follow_up, turn_end or next_turn"),
                };
            }
            _ => anyhow::bail!("use --code-mode, --deny and/or --delivery"),
        };
        i += 1;
    }
    println!("{}", support::run(mode, allow, delivery).await?);
    Ok(())
}
