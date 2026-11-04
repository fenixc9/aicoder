//! Shared native transport binding across primary and auxiliary clients; loopback/fake keys only.
#[path = "support/auxiliary_transport.rs"]
mod support;
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        args.iter()
            .all(|a| matches!(a.as_str(), "--code-mode" | "--deny" | "--observer-error")),
        "use --code-mode/--deny/--observer-error"
    );
    println!(
        "{}",
        support::run(
            args.iter().any(|a| a == "--code-mode"),
            !args.iter().any(|a| a == "--deny"),
            args.iter().any(|a| a == "--observer-error")
        )
        .await?
    );
    Ok(())
}
