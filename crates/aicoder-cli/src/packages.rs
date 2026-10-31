use aicoder_runtime::{Runtime, packages::*};
use anyhow::Result;
use clap::Subcommand;
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
pub fn execute(runtime: &Runtime, command: &PackageCommand) -> Result<()> {
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
                PackageCommand::Inspect { .. } | PackageCommand::Lock => unreachable!(),
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
