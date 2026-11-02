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
    /// Enable bounded JavaScript tool composition (requires the code-mode build feature).
    #[arg(long)]
    code_mode: bool,
    #[arg(long)]
    extension_config: Option<PathBuf>,
    #[arg(long)]
    activate_packages: bool,
    #[arg(long, requires = "activate_packages")]
    package_programs: Option<PathBuf>,
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
                code_mode: aicoder_runtime::code_mode::CodeModeSettings {
                    enabled: if cli.code_mode {
                        Setting::Value(true)
                    } else {
                        Setting::Inherit
                    },
                    ..Default::default()
                },
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
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let activation = if cli.extension_config.is_some() || cli.activate_packages {
        use aicoder_runtime::host_extensions::{HostExtensionManifest, HostProgramManifest};
        let manifest = cli
            .extension_config
            .as_ref()
            .map(HostExtensionManifest::read)
            .transpose()?
            .unwrap_or_default();
        let programs = if cli.activate_packages {
            Some(
                cli.package_programs
                    .as_ref()
                    .map(HostProgramManifest::read)
                    .transpose()?
                    .unwrap_or_default(),
            )
        } else {
            None
        };
        let context = aicoder_core::TurnExecutionContext::new();
        let work = runtime.activate_host_extensions(&manifest, programs.as_ref(), context.clone());
        tokio::pin!(work);
        let result = loop {
            tokio::select! {biased;
                _=tokio::signal::ctrl_c()=>context.cancel("Web startup cancelled"),
                _=term.recv()=>context.cancel("Web startup terminated"),
                result=&mut work=>break result,
            }
        };
        Some(result?)
    } else {
        None
    };
    let runtime = if let Some(active) = &activation {
        runtime.with_extensions(active.extensions())?
    } else {
        runtime
    };
    let outcome=async {
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

    Ok::<_,anyhow::Error>(())
    }.await;
    match activation {
        Some(active) => active.finish(outcome).await,
        None => outcome,
    }
}
