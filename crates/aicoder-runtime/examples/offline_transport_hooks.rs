//! Actual compatible HTTP stages with core request views and sessions; loopback/fake credentials only.
#[path = "support/transport_hooks.rs"]
mod support;
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        args.iter().all(|a| a == "--stream"),
        "use --stream optionally"
    );
    println!(
        "{}",
        support::run(args.iter().any(|a| a == "--stream")).await?
    );
    Ok(())
}
