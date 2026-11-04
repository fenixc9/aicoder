//! Native argument/result transformations over real runtime tools and sessions, entirely offline.
#[path = "support/tool_interception.rs"]
mod support;
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        args.iter()
            .all(|a| matches!(a.as_str(), "--code-mode" | "--deny")),
        "use --code-mode and/or --deny"
    );
    println!(
        "{}",
        support::run(
            args.iter().any(|a| a == "--code-mode"),
            !args.iter().any(|a| a == "--deny")
        )
        .await?
    );
    Ok(())
}
