use std::path::PathBuf;

use aicoder_core::tools::AllowAllApproval;
use aicoder_eval::{
    SweBenchAdapter, SweBenchBatchOptions, SweBenchBatchRunner, SweBenchDataset, SweBenchFilter,
    SweBenchHarnessConfig, SweBenchHarnessReport, SweBenchRepositoryCache,
    SweBenchRepositorySource, SweBenchScore, run_swebench_harness_with_context,
};
use aicoder_runtime::resources::ResourceSettings;
use aicoder_runtime::verification::WorkspaceChangeVerifier;
use aicoder_runtime::{
    ConfigDiscovery, ExecutionSettings, ModelSettings, Runtime, RuntimeOverrides, Setting,
    Settings, environment_file,
};
use anyhow::{Result, ensure};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use std::sync::Arc;

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
    Run(Box<RunArgs>),
    /// Run the official Python/Docker harness for generated predictions.
    Grade(Box<GradeArgs>),
    /// Read and normalize an official harness aggregate report.
    Import { report: PathBuf },
    /// Verify a complete generation plan, selected denominator and artifact references.
    VerifyGeneration { run: PathBuf },
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
    #[arg(long)]
    max_rounds: Option<usize>,
    #[arg(long)]
    max_tool_calls: Option<usize>,
    #[arg(long)]
    token_budget: Option<u64>,
    #[arg(long)]
    time_budget_ms: Option<u64>,
    #[arg(long)]
    output_budget_bytes: Option<usize>,
    #[arg(long)]
    max_tokens: Option<u32>,
    #[arg(long)]
    temperature: Option<f32>,
    #[arg(long, action = clap::ArgAction::Set)]
    stream: Option<bool>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, conflicts_with = "no_config")]
    config: Option<PathBuf>,
    #[arg(long, conflicts_with = "no_config")]
    user_config: Option<PathBuf>,
    #[arg(long)]
    no_config: bool,
    #[arg(long)]
    env_file: Option<PathBuf>,
    #[arg(long)]
    no_resources: bool,
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
    #[arg(long)]
    generation_run: Option<PathBuf>,
    #[arg(long, default_value_t = 7200)]
    total_timeout: u64,
    #[arg(long = "exclude-env")]
    excluded_environment: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().command {
        Command::Run(arguments) => run(*arguments).await,
        Command::Grade(arguments) => grade(*arguments).await,
        Command::Import { report } => import(report),
        Command::VerifyGeneration { run } => {
            let report = aicoder_eval::SweBenchBatchReport::load_verified(run)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"verified":true,"plan_sha256":report.plan_sha256,"predictions_sha256":report.predictions_sha256,"summary":report.summary})
                )?
            );
            Ok(())
        }
    }
}

async fn run(arguments: RunArgs) -> Result<()> {
    ensure!(arguments.workers > 0, "--workers must be positive");
    let runtime = Arc::new(Runtime::load_with_environment(
        std::env::current_dir()?,
        RuntimeOverrides {
            settings: Settings {
                provider_profile: Setting::from_option(arguments.profile.clone()),
                model: Setting::from_option(arguments.model.clone()),
                resources: ResourceSettings {
                    enabled: if arguments.no_resources {
                        Setting::Value(false)
                    } else {
                        Setting::Inherit
                    },
                    ..Default::default()
                },
                options: ModelSettings {
                    max_output_tokens: Setting::from_option(arguments.max_tokens),
                    temperature: Setting::from_option(arguments.temperature),
                    ..Default::default()
                },
                execution: ExecutionSettings {
                    max_rounds: Setting::from_option(arguments.max_rounds),
                    stream: Setting::from_option(arguments.stream),
                    max_tool_calls: Setting::from_option(arguments.max_tool_calls),
                    max_tokens: Setting::from_option(arguments.token_budget),
                    max_duration_ms: Setting::from_option(arguments.time_budget_ms),
                    max_output_bytes: Setting::from_option(arguments.output_budget_bytes),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: !arguments.no_config,
            user_file: arguments.user_config.clone(),
            project_file: arguments.config.clone(),
        },
        environment_file(arguments.env_file.as_deref())?,
    )?);
    // Fail before repository downloads when the default credential reference cannot be resolved.
    runtime.credential_provider().bearer_token().await?;
    let model = runtime.config().provider.model.clone();
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
    let cache_root = arguments.repository_cache.unwrap_or_else(|| {
        arguments
            .output
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(".aicoder-swe-repositories")
    });
    let context = aicoder_core::TurnExecutionContext::new();
    let signal = context.clone();
    let signal_owner = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel("SWE-bench SIGINT");
        }
    });
    let cache = SweBenchRepositoryCache::new(&cache_root);
    cache
        .prepare_with_context(
            &selected.instances,
            context.clone(),
            &runtime.config().credential_references,
        )
        .await?;
    let adapter = SweBenchAdapter::new(&model)
        .model_name_or_path(&model_name_or_path)
        .repository_source(SweBenchRepositorySource::LocalRoot(cache_root))
        .include_hints(!arguments.no_hints)
        .model_options(runtime.config().options.clone())
        .prepend_system_prompt(
            runtime
                .config()
                .system_prompt
                .as_deref()
                .unwrap_or_default(),
        );
    let parameters = json!({
        "runner_version":env!("CARGO_PKG_VERSION"),
        "runner_executable_sha256":aicoder_eval::benchmark::executable_fingerprint()?,
        "dataset_path":arguments.dataset,
        "selected_instance_ids":selected.instances.iter().map(|i|&i.instance_id).collect::<Vec<_>>(),
        "include_hints":!arguments.no_hints,
        "runtime":runtime.config(),
        "global_resources":runtime.global_resources()?.manifest(),
        "resource_scope":"isolated_case_workspace_with_frozen_globals",
        "require_workspace_change":!arguments.allow_empty_patch,
    });
    let execution_pin =
        ring::digest::digest(&ring::digest::SHA256, &serde_json::to_vec(&parameters)?)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
    let options = SweBenchBatchOptions::new(&arguments.output, &arguments.run_id)
        .dataset(selected.name.clone())
        .concurrency(arguments.workers)
        .resume(!arguments.no_resume)
        .parameters(parameters)
        .execution_pin(execution_pin);
    let cases = adapter.adapt_dataset(&selected)?;
    let provider = Arc::new(runtime.client()?);
    let allow_empty_patch = arguments.allow_empty_patch;
    let excluded = runtime.config().credential_references.clone();
    let outcome = SweBenchBatchRunner::new(adapter, options)
        .eval_runner(aicoder_eval::EvalRunner::new().excluding_environment(excluded))
        .run_with_context(
            cases,
            move |workspace| {
                let builder = runtime
                    .executor_builder_for(provider.clone(), workspace)?
                    .approval(AllowAllApproval);
                if allow_empty_patch {
                    builder.build()
                } else {
                    builder
                        .completion_verifier(WorkspaceChangeVerifier::new())
                        .build()
                }
            },
            context.clone(),
        )
        .await;
    signal_owner.abort();
    let _ = signal_owner.await;
    let report = outcome?;
    println!("{}", serde_json::to_string_pretty(&report.summary)?);
    if context.is_cancelled() {
        std::process::exit(130);
    }
    ensure!(
        report.summary.failed == 0 && report.summary.incomplete == 0,
        "{} SWE-bench cases failed and {} were incomplete; resume after correcting the cause or changing the run configuration",
        report.summary.failed,
        report.summary.incomplete
    );
    Ok(())
}

async fn grade(arguments: GradeArgs) -> Result<()> {
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
    config.generation_manifest = arguments.generation_run;
    config.total_timeout_seconds = arguments.total_timeout;
    config.excluded_environment = arguments.excluded_environment;
    let context = aicoder_core::TurnExecutionContext::new();
    let signal = context.clone();
    let owner = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel("SWE-bench grading SIGINT");
        }
    });
    let result = run_swebench_harness_with_context(&config, context.clone()).await;
    owner.abort();
    let _ = owner.await;
    if context.is_cancelled() {
        eprintln!("Grading cancelled and owned process cleanup awaited; no success was published");
        std::process::exit(130);
    }
    let execution = result?;
    eprint!("{}", String::from_utf8_lossy(&execution.output.stdout));
    eprint!("{}", String::from_utf8_lossy(&execution.output.stderr));
    println!("{}", serde_json::to_string_pretty(&execution.score)?);
    Ok(())
}

fn import(path: PathBuf) -> Result<()> {
    if path.is_dir() || path.file_name().is_some_and(|name| name == "score.json") {
        let directory = if path.is_dir() {
            path.as_path()
        } else {
            path.parent().unwrap_or_else(|| std::path::Path::new("."))
        };
        let score = SweBenchScore::load_verified(directory)?;
        println!("{}", serde_json::to_string_pretty(&score)?);
        return Ok(());
    }
    let report = SweBenchHarnessReport::load(path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "resolved_rate": report.resolved_rate(),
            "submitted_resolved_rate":report.submitted_resolved_rate(),
            "denominator":"official_total_instances",
            "report": report,
        }))?
    );
    Ok(())
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
        assert_eq!(arguments.stream, Some(false));
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
