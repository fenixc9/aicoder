//! Explicit custom-message storage/idle runs using actual runtime tools and JSONL, offline.
#[path = "support/custom_messages.rs"]
mod support;
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        args.iter()
            .all(|a| matches!(a.as_str(), "--code-mode" | "--deny" | "--record-only")),
        "use --code-mode, --deny and/or --record-only"
    );
    println!(
        "{}",
        support::run(
            args.iter().any(|a| a == "--code-mode"),
            !args.iter().any(|a| a == "--deny"),
            args.iter().any(|a| a == "--record-only")
        )
        .await?
    );
    Ok(())
}
