use aicoder_runtime::{
    ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings, environment_file,
    resources::ResourceSettings,
};
use aicoder_web::{WebHost, WebOptions};
use anyhow::Result;
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf, time::Duration};
#[derive(Parser)]
#[command(about = "Optional fixed-workspace loopback Web example")]
struct Cli {
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    #[arg(long, default_value = "127.0.0.1:3000")]
    listen: SocketAddr,
    #[arg(long)]
    no_session: bool,
    #[arg(long)]
    no_config: bool,
    #[arg(long)]
    no_resources: bool,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    reasoning_effort: Option<aicoder_runtime::ReasoningOverride>,
    #[arg(long)]
    env_file: Option<PathBuf>,
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    anyhow::ensure!(
        cli.listen.ip().is_loopback(),
        "Web example only binds loopback addresses"
    );
    let token = std::env::var("AICODER_WEB_TOKEN")
        .map_err(|_| anyhow::anyhow!("Set AICODER_WEB_TOKEN to a private random token"))?;
    let listener = tokio::net::TcpListener::bind(cli.listen).await?;
    let address = listener.local_addr()?;
    let mut options = WebOptions::new(token, address)?;
    if cli.no_session {
        options = options.memory_sessions();
    }
    let runtime = Runtime::load_with_environment(
        &cli.workspace,
        RuntimeOverrides {
            settings: Settings {
                model: Setting::from_option(cli.model),
                options: aicoder_runtime::ModelSettings {
                    reasoning_effort: cli
                        .reasoning_effort
                        .map(|v| v.setting())
                        .unwrap_or(Setting::Inherit),
                    ..Default::default()
                },
                provider_profile: Setting::from_option(cli.profile),
                resources: ResourceSettings {
                    enabled: if cli.no_resources {
                        Setting::Value(false)
                    } else {
                        Setting::Inherit
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: if cli.no_session {
                Setting::Clear
            } else {
                Setting::Inherit
            },
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: !cli.no_config,
            ..Default::default()
        },
        environment_file(cli.env_file.as_deref())?,
    )?;
    let host = WebHost::new(runtime, options)?;
    let router = host.router();
    let (close, closed) = tokio::sync::oneshot::channel::<()>();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = closed.await;
            })
            .await
    });
    eprintln!("Web example listening at http://{address}");
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let ended = tokio::select! {result=&mut server=>Some(result),_=tokio::signal::ctrl_c()=>None,_=term.recv()=>None};
    host.shutdown().await;
    let _ = close.send(());
    if let Some(result) = ended {
        result??;
    } else {
        match tokio::time::timeout(Duration::from_secs(5), &mut server).await {
            Ok(result) => result??,
            Err(_) => {
                server.abort();
                let _ = server.await;
            }
        }
    }
    Ok(())
}
