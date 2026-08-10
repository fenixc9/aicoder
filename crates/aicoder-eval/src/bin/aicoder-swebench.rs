use std::path::PathBuf;

use aicoder_core::{
    Agent, AgentConfig, ChatClient, WorkspaceChangeVerifier, tools::AllowAllApproval,
};
use aicoder_eval::{
    SweBenchAdapter, SweBenchBatchOptions, SweBenchBatchRunner, SweBenchDataset, SweBenchFilter,
    SweBenchHarnessConfig, SweBenchHarnessReport, SweBenchRepositoryCache,
    SweBenchRepositorySource, run_swebench_harness,
};
use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use serde_json::json;

#[derive(Debug, Parser)]
#[command(
    name = "aicoder-swebench",
    about = "Run reproducible aicoder SWE-bench evaluations"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate patches for a local SWE-bench JSON/JSONL dataset.
    Run(RunArgs),
    /// Run the official Python/Docker harness for generated predictions.
    Grade(GradeArgs),
    /// Read and normalize an official harness aggregate report.
    Import { report: PathBuf },
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(long)]
    dataset: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    model_name_or_path: Option<String>,
    #[arg(long = "instance-id")]
    instance_ids: Vec<String>,
    #[arg(long = "repo")]
    repositories: Vec<String>,
    #[arg(long)]
    difficulty: Option<String>,
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long, default_value_t = 1)]
    workers: usize,
    #[arg(long)]
    repository_cache: Option<PathBuf>,
    #[arg(long)]
    no_resume: bool,
    #[arg(long)]
    no_hints: bool,
    #[arg(long, default_value_t = 8)]
    max_rounds: usize,
    #[arg(long, default_value_t = 4096)]
    max_tokens: i32,
    #[arg(long, default_value_t = 0.0)]
    temperature: f32,
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    stream: bool,
    /// Permit a final answer even when the agent produced no workspace changes.
    #[arg(long)]
    allow_empty_patch: bool,
}

#[derive(Debug, Args)]
struct GradeArgs {
    #[arg(long)]
    predictions: PathBuf,
    #[arg(long, default_value = "princeton-nlp/SWE-bench_Lite")]
    dataset_name: String,
    #[arg(long, default_value = "test")]
    split: String,
    #[arg(long)]
    run_id: String,
    #[arg(long, default_value_t = 1)]
    workers: usize,
    #[arg(long, default_value_t = 1800)]
    timeout: u64,
    #[arg(long, default_value = ".")]
    report_dir: PathBuf,
    #[arg(long, default_value = "python")]
    python: PathBuf,
    #[arg(long = "instance-id")]
    instance_ids: Vec<String>,
    #[arg(long)]
    modal: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    load_dotenv()?;
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();
    match Cli::parse().command {
        Command::Run(arguments) => run(arguments).await,
        Command::Grade(arguments) => grade(arguments),
        Command::Import { report } => import(report),
    }
}

async fn run(arguments: RunArgs) -> Result<()> {
    ensure!(arguments.workers > 0, "--workers must be positive");
    ensure!(arguments.max_rounds > 0, "--max-rounds must be positive");
    let model = arguments
        .model
        .or_else(|| std::env::var("OPENAI_MODEL").ok())
        .context("Set --model or OPENAI_MODEL")?;
    let model_name_or_path = arguments
        .model_name_or_path
        .unwrap_or_else(|| model.clone());
    let dataset = SweBenchDataset::load(&arguments.dataset)?;
    let selected = dataset.filtered(&SweBenchFilter {
        instance_ids: arguments.instance_ids,
        repositories: arguments.repositories,
        difficulty: arguments.difficulty,
        limit: arguments.limit,
    })?;
    let cache_root = arguments
        .repository_cache
        .unwrap_or_else(|| arguments.output.join("repositories"));
    let cache = SweBenchRepositoryCache::new(&cache_root);
    cache.prepare(&selected.instances)?;
    let adapter = SweBenchAdapter::new(&model)
        .model_name_or_path(&model_name_or_path)
        .repository_source(SweBenchRepositorySource::LocalRoot(cache_root))
        .include_hints(!arguments.no_hints)
        .temperature(Some(arguments.temperature))
        .max_tokens(Some(arguments.max_tokens));
    let options = SweBenchBatchOptions::new(&arguments.output, &arguments.run_id)
        .dataset(selected.name.clone())
        .concurrency(arguments.workers)
        .resume(!arguments.no_resume)
        .parameters(json!({
            "dataset_path": arguments.dataset,
            "selected_instance_ids": selected.instances.iter().map(|item| &item.instance_id).collect::<Vec<_>>(),
            "include_hints": !arguments.no_hints,
            "temperature": arguments.temperature,
            "max_tokens": arguments.max_tokens,
            "max_rounds": arguments.max_rounds,
            "stream": arguments.stream,
            "require_workspace_change": !arguments.allow_empty_patch,
        }));
    let cases = adapter.adapt_dataset(&selected)?;
    let agent_model = model.clone();
    let max_rounds = arguments.max_rounds;
    let stream = arguments.stream;
    let allow_empty_patch = arguments.allow_empty_patch;
    let report = SweBenchBatchRunner::new(adapter, options)
        .run(cases, move |workspace| {
            let client = ChatClient::from_env(&agent_model)?;
            let builder = Agent::builder(client)
                .workspace(workspace)
                .approval(AllowAllApproval)
                .config(AgentConfig { max_rounds, stream });
            if allow_empty_patch {
                builder.build()
            } else {
                builder
                    .completion_verifier(WorkspaceChangeVerifier::new())
                    .build()
            }
        })
        .await?;
    println!("{}", serde_json::to_string_pretty(&report.summary)?);
    ensure!(
        report.summary.failed == 0 && report.summary.incomplete == 0,
        "{} SWE-bench cases failed and {} were incomplete; resume after correcting the cause or changing the run configuration",
        report.summary.failed,
        report.summary.incomplete
    );
    Ok(())
}

fn grade(arguments: GradeArgs) -> Result<()> {
    let mut config = SweBenchHarnessConfig::new(
        arguments.dataset_name,
        arguments.predictions,
        arguments.run_id,
    );
    config.python = arguments.python;
    config.split = arguments.split;
    config.instance_ids = arguments.instance_ids;
    config.max_workers = arguments.workers;
    config.timeout_seconds = arguments.timeout;
    config.report_dir = arguments.report_dir;
    config.modal = arguments.modal;
    let execution = run_swebench_harness(&config)?;
    print!("{}", String::from_utf8_lossy(&execution.output.stdout));
    if let Some(report) = execution.report {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

fn import(path: PathBuf) -> Result<()> {
    let report = SweBenchHarnessReport::load(path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "resolved_rate": report.resolved_rate(),
            "report": report,
        }))?
    );
    Ok(())
}

fn load_dotenv() -> Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => Ok(()),
        Err(error) if error.not_found() => Ok(()),
        Err(error) => Err(error).context("Failed to load .env"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_accepts_reproducibility_controls() {
        let cli = Cli::try_parse_from([
            "aicoder-swebench",
            "run",
            "--dataset",
            "dataset.jsonl",
            "--output",
            "artifacts",
            "--run-id",
            "baseline-1",
            "--model",
            "model",
            "--instance-id",
            "owner__repo-1",
            "--workers",
            "2",
            "--stream=false",
        ])
        .unwrap();
        let Command::Run(arguments) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(arguments.workers, 2);
        assert!(!arguments.stream);
        assert!(!arguments.allow_empty_patch);
        assert_eq!(arguments.instance_ids, ["owner__repo-1"]);
    }

    #[test]
    fn grade_command_defaults_to_official_lite_test_split() {
        let cli = Cli::try_parse_from([
            "aicoder-swebench",
            "grade",
            "--predictions",
            "predictions.json",
            "--run-id",
            "baseline-1",
        ])
        .unwrap();
        let Command::Grade(arguments) = cli.command else {
            panic!("expected grade command");
        };
        assert_eq!(arguments.dataset_name, "princeton-nlp/SWE-bench_Lite");
        assert_eq!(arguments.split, "test");
    }
}
