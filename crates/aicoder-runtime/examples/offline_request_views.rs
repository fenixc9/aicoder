//! Request view transforms through the real HTTP adapter; loopback only and no real credentials.
#[path = "support/request_views.rs"]
mod support;
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        args.iter().all(|v| v == "--stream"),
        "use --stream optionally"
    );
    println!(
        "{}",
        support::run(args.iter().any(|v| v == "--stream")).await?
    );
    Ok(())
}
