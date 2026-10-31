#[tokio::main]
async fn main() -> anyhow::Result<()> {
    aicoder_tui::run_cli().await
}
