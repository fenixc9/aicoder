//! Raw input interception before templates, actual runtime tools and JSONL, entirely offline.
#[path = "support/input_hooks.rs"]
mod support;
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        args.iter()
            .all(|a| matches!(a.as_str(), "--code-mode" | "--deny" | "--handled")),
        "use --code-mode, --deny and/or --handled"
    );
    println!(
        "{}",
        support::run(
            args.iter().any(|a| a == "--code-mode"),
            !args.iter().any(|a| a == "--deny"),
            args.iter().any(|a| a == "--handled")
        )
        .await?
    );
    Ok(())
}
