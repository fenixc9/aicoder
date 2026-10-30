use super::*;
use aicoder_core::TurnExecutionContext;
use aicoder_runtime::exports::{self, DirectoryPublisher, ExportLimits, ExportPublisher};
enum Destination {
    Local(PathBuf),
    Publish(DirectoryPublisher),
}
pub async fn handle(cli: &Cli, runtime: &Runtime) -> Result<bool> {
    let (id, format, limits, destination) = match &cli.command {
        Some(Command::ExportSession {
            id,
            output,
            format,
            limits,
        }) => (
            id.clone(),
            *format,
            limits,
            Destination::Local(output.clone()),
        ),
        Some(Command::ShareSession {
            id,
            directory,
            base_url,
            format,
            limits,
        }) => (
            id.clone(),
            *format,
            limits,
            Destination::Publish(DirectoryPublisher::new(directory, base_url.as_deref())?),
        ),
        _ => return Ok(false),
    };
    let limits = ExportLimits {
        max_source_bytes: limits.max_source_bytes,
        max_entries: limits.max_entries,
        max_output_bytes: limits.max_bytes,
    }
    .validate()?;
    let repository = runtime.sessions()?;
    let workspace = runtime.config().workspace.clone();
    let context = TurnExecutionContext::new();
    let owned = context.clone();
    let work = async move {
        let preparing = owned.clone();
        let export = tokio::task::spawn_blocking(move || {
            exports::prepare(&repository, &workspace, &id, format, limits, &preparing)
        })
        .await??;
        match destination {
            Destination::Local(path) => {
                tokio::task::spawn_blocking(move || export.write_new(&path, &owned)).await?
            }
            Destination::Publish(publisher) => publisher.publish(&export, owned).await,
        }
    };
    let result: Result<aicoder_runtime::exports::PublishedExport> =
        execution::cancellable(context.clone(), work).await?;
    match result {
        Ok(receipt) => println!("{}", serde_json::to_string_pretty(&receipt)?),
        Err(error) if context.is_cancelled() => {
            eprintln!("{error:#}");
            return Err(input::Interrupted.into());
        }
        Err(error) => return Err(error),
    }
    if context.is_cancelled() {
        return Err(input::Interrupted.into());
    }
    Ok(true)
}
