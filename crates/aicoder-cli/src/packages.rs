use aicoder_runtime::{Runtime, packages::*};
use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;
#[derive(Debug, Subcommand)]
pub enum PackageCommand {
    /// List installed content pins, active revisions and enabled roots.
    List,
    /// Validate and print source metadata without installing or executing it.
    Inspect {
        path: PathBuf,
    },
    /// Import allowlisted files into trusted host storage; never runs scripts.
    Install {
        path: PathBuf,
        /// Trust executable declarations for this exact content pin.
        #[arg(long)]
        allow_execution: bool,
        #[arg(long)]
        disabled: bool,
    },
    /// Fetch a Git revision in a bounded private quarantine; never runs package code.
    InstallGit {
        url: String,
        #[arg(long, default_value = "HEAD")]
        revision: String,
        #[command(flatten)]
        options: RemoteOptions,
    },
    /// Resolve an npm version/tag, verify its tarball and import native package resources.
    InstallNpm {
        package: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long, default_value = "https://registry.npmjs.org/")]
        registry: String,
        #[command(flatten)]
        options: RemoteOptions,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    /// Revoke future selection; existing snapshots retain their objects.
    Remove {
        id: String,
    },
    /// Select an already trusted exact revision.
    Select {
        id: String,
        version: String,
        sha256: String,
    },
    /// Emit exact selection as a portable public state/lock document.
    Lock,
}
#[derive(Debug, Args)]
pub struct RemoteOptions {
    #[arg(long)]
    allow_execution: bool,
    #[arg(long)]
    disabled: bool,
    /// Host environment reference bound only to this source origin; never a token value.
    #[arg(long)]
    token_env: Option<String>,
    /// Explicit local fixture/self-host policy; arbitrary unencrypted HTTP remains rejected.
    #[arg(long)]
    allow_loopback_http: bool,
    #[arg(long, default_value_t = 120)]
    timeout: u64,
}
pub async fn execute(runtime: &Runtime, command: &PackageCommand) -> Result<()> {
    if matches!(
        command,
        PackageCommand::InstallGit { .. } | PackageCommand::InstallNpm { .. }
    ) {
        return remote(runtime, command).await;
    }
    let output = match command {
        PackageCommand::Inspect { path } => serde_json::to_value(PackageStore::inspect(path)?)?,
        PackageCommand::Lock => serde_json::to_value(PackageSessionState::capture(
            runtime.package_snapshot()?.as_ref(),
        ))?,
        command => {
            let store = runtime.package_store()?;
            match command {
                PackageCommand::List => serde_json::to_value(store.index()?)?,
                PackageCommand::Install {
                    path,
                    allow_execution,
                    disabled,
                } => serde_json::to_value(store.install_local(
                    path,
                    InstallOptions {
                        allow_execution: *allow_execution,
                        enable: !disabled,
                    },
                    &aicoder_core::TurnExecutionContext::new(),
                )?)?,
                PackageCommand::Enable { id } => {
                    store.enable(id, true)?;
                    serde_json::to_value(store.index()?)?
                }
                PackageCommand::Disable { id } => {
                    store.enable(id, false)?;
                    serde_json::to_value(store.index()?)?
                }
                PackageCommand::Remove { id } => {
                    store.uninstall(id)?;
                    serde_json::to_value(store.index()?)?
                }
                PackageCommand::Select {
                    id,
                    version,
                    sha256,
                } => {
                    store.select_revision(&PackagePin {
                        id: id.clone(),
                        version: version.clone(),
                        sha256: sha256.clone(),
                    })?;
                    serde_json::to_value(store.index()?)?
                }
                PackageCommand::Inspect { .. }
                | PackageCommand::Lock
                | PackageCommand::InstallGit { .. }
                | PackageCommand::InstallNpm { .. } => unreachable!(),
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
#[cfg(not(feature = "package-downloads"))]
async fn remote(_: &Runtime, _: &PackageCommand) -> Result<()> {
    anyhow::bail!("remote packages require a build with the package-downloads feature")
}
#[cfg(feature = "package-downloads")]
async fn remote(runtime: &Runtime, command: &PackageCommand) -> Result<()> {
    let (request, origin, options) = match command {
        PackageCommand::InstallGit {
            url,
            revision,
            options,
        } => (
            PackageRequest::Git {
                url: url.clone(),
                revision: revision.clone(),
            },
            url.as_str(),
            options,
        ),
        PackageCommand::InstallNpm {
            package,
            version,
            registry,
            options,
        } => (
            PackageRequest::Npm {
                registry: registry.clone(),
                package: package.clone(),
                version: version.clone(),
            },
            registry.as_str(),
            options,
        ),
        _ => unreachable!(),
    };
    let mut limits = PackageAcquireLimits::default();
    limits.timeout = std::time::Duration::from_secs(options.timeout);
    let mut acquirer = PackageAcquirer::for_runtime(runtime, limits)?
        .with_loopback_http(options.allow_loopback_http);
    if let Some(reference) = &options.token_env {
        acquirer = acquirer.with_bearer_reference(origin, reference)?;
    }
    let context = aicoder_core::TurnExecutionContext::new();
    let store = runtime.package_store()?;
    let result = crate::execution::cancellable(
        context.clone(),
        acquirer.install(
            &store,
            &request,
            InstallOptions {
                allow_execution: options.allow_execution,
                enable: !options.disabled,
            },
            context,
        ),
    )
    .await??;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
