//! Direct SDK benchmark entry; real providers are used only with explicit --live.
use aicoder_core::{TurnExecutionContext, model::*, tools::AllowAllApproval};
use aicoder_eval::benchmark::*;
use aicoder_runtime::{packages::PackageSettings, resources::ResourceSettings, *};
use anyhow::{Result, ensure};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
#[derive(Parser)]
#[command(
    name = "aicoder-bench",
    about = "Reproducible coding cases over core/runtime; defaults to offline contracts"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Run(Box<Run>),
    /// Capture source/compiler/executable metadata before entering an isolated executor.
    ObserveSource {
        #[arg(long, default_value = ".")]
        source_root: PathBuf,
    },
    Verify {
        archive: PathBuf,
    },
    Replay {
        archive: PathBuf,
        #[arg(long)]
        case: String,
        #[arg(long, default_value_t = 0)]
        repetition: usize,
    },
    Compare {
        left: PathBuf,
        right: PathBuf,
    },
    List {
        #[arg(long)]
        suite: Option<PathBuf>,
    },
}
#[derive(Args)]
struct Run {
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    suite: Option<PathBuf>,
    #[arg(long, default_value = ".")]
    source_root: PathBuf,
    /// Explicit trusted pre-launch observation; must match this executable/platform/compiler.
    #[arg(long)]
    source_observation: Option<PathBuf>,
    #[arg(long = "case")]
    cases: Vec<String>,
    #[arg(long, default_value_t = 1)]
    repetitions: usize,
    /// Explicitly authorize network/model usage using normal trusted provider configuration.
    #[arg(long)]
    live: bool,
    #[arg(long,value_enum,default_value_t=ScriptVariant::Correct)]
    variant: ScriptVariant,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    seed: Option<i64>,
    #[arg(long)]
    user_config: Option<PathBuf>,
    #[arg(long)]
    env_file: Option<PathBuf>,
    #[arg(long, default_value_t = 8)]
    max_rounds: usize,
    #[arg(long, default_value_t = 16)]
    max_tool_calls: usize,
    #[arg(long, default_value_t = 32000)]
    token_budget: u64,
    #[arg(long, default_value_t = 30000)]
    time_budget_ms: u64,
    #[arg(long, default_value_t = 1048576)]
    output_budget_bytes: usize,
}
fn suite(path: Option<PathBuf>) -> Result<CodingSuite> {
    match path {
        Some(path) => CodingSuite::load(path),
        None => CodingSuite::builtin(),
    }
}
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run(args) => run(*args).await,
        Command::ObserveSource { source_root } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&SourceStamp::capture(source_root)?)?
            );
            Ok(())
        }
        Command::Verify { archive } => {
            let report = BenchReport::verify(archive)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"verified":true,"mode":report.mode,"source":report.source,"summary":report.summary})
                )?
            );
            Ok(())
        }
        Command::Replay {
            archive,
            case,
            repetition,
        } => {
            let trace = BenchReport::replay_case(archive, &case, repetition)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"observation_only":true,"events":trace.events.len(),"trajectory":trace.summary()})
                )?
            );
            Ok(())
        }
        Command::Compare { left, right } => {
            let compared = BenchComparison::compare(
                &BenchReport::verify(left)?,
                &BenchReport::verify(right)?,
            )?;
            println!("{}", serde_json::to_string_pretty(&compared)?);
            Ok(())
        }
        Command::List { suite: path } => {
            let suite = suite(path)?;
            let cases=suite.cases.iter().map(|c|json!({"id":c.id,"prompt":c.prompt,"initial_files":c.files.keys().collect::<Vec<_>>()})).collect::<Vec<_>>();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"id":suite.id,"fingerprint":suite.fingerprint()?,"cases":cases})
                )?
            );
            Ok(())
        }
    }
}
async fn run(args: Run) -> Result<()> {
    ensure!(
        args.live || (args.user_config.is_none() && args.env_file.is_none()),
        "provider configuration requires explicit --live"
    );
    ensure!(
        !args.live || (args.model.is_some() && args.variant == ScriptVariant::Correct),
        "live runs require --model and cannot select synthetic failure variants"
    );
    ensure!(
        (1..=64).contains(&args.max_rounds)
            && (1..=512).contains(&args.max_tool_calls)
            && (1..=600000).contains(&args.time_budget_ms),
        "benchmark execution bounds exceeded"
    );
    let source = match &args.source_observation {
        Some(path) => SourceStamp::load_observation(path)?,
        None => SourceStamp::capture(&args.source_root)?,
    };
    let suite = suite(args.suite)?;
    let model = args.model.unwrap_or_else(|| "offline-coding-v1".into());
    let settings = Settings {
        model: Setting::Value(model.clone()),
        options: ModelSettings {
            seed: Setting::from_option(args.seed),
            max_output_tokens: Setting::Value(2048),
            ..Default::default()
        },
        execution: ExecutionSettings {
            max_rounds: Setting::Value(args.max_rounds),
            max_tool_calls: Setting::Value(args.max_tool_calls),
            max_tokens: Setting::Value(args.token_budget),
            max_duration_ms: Setting::Value(args.time_budget_ms),
            max_output_bytes: Setting::Value(args.output_budget_bytes),
            stream: Setting::Value(false),
            ..Default::default()
        },
        resources: ResourceSettings {
            enabled: Setting::Value(false),
            ..Default::default()
        },
        packages: PackageSettings {
            enabled: Setting::Value(false),
            ..Default::default()
        },
        ..Default::default()
    };
    let environment: Arc<dyn Environment> = if args.live {
        environment_file(args.env_file.as_deref())?
    } else {
        Arc::new(BTreeMap::<String, String>::new())
    };
    let runtime = Runtime::load_with_environment(
        &args.source_root,
        RuntimeOverrides {
            settings,
            session_dir: Setting::Clear,
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: args.user_config.is_some(),
            user_file: args.user_config,
            project_file: None,
        },
        environment,
    )?;
    let live: Option<Arc<dyn ModelProvider>> = if args.live {
        runtime.credential_provider().bearer_token().await?;
        Some(Arc::new(runtime.client()?))
    } else {
        None
    };
    let mut effective_options = runtime.config().options.clone();
    if effective_options.seed.is_none()
        && live.as_ref().map_or(Ok(true), |provider| {
            provider
                .model_spec(&model)
                .map(|spec| spec.capabilities.seed)
        })?
    {
        effective_options.seed = Some(0);
    }
    let mut options = BenchOptions::new(
        &args.output,
        source,
        if args.live {
            BenchMode::LiveProvider
        } else {
            BenchMode::ScriptedContract
        },
    );
    options.repetitions = args.repetitions;
    options.selected_ids = args.cases;
    options.host_parameters = json!({"runtime":runtime.config(),"approval":"explicit_benchmark_allow_all","provider_kind":if args.live {"live"}else{"scripted_contract"},"variant":args.variant,"resources":"disabled","concurrency":1,"effective_model_options":effective_options});
    if args.source_observation.is_some() {
        options.host_parameters["source_observation"] =
            json!("explicit_host_prelaunch_observation");
    }
    options.excluded_environment = runtime.config().credential_references.clone();
    let runner = BenchRunner::new(suite, options)?;
    let context = TurnExecutionContext::new();
    let signal = context.clone();
    let owner = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel("benchmark SIGINT");
        }
    });
    let outcome = runner
        .run(
            |case| {
                case.request(
                    &model,
                    effective_options.clone(),
                    runtime.config().system_prompt.as_deref(),
                )
            },
            |case, workspace| {
                let provider = live
                    .clone()
                    .unwrap_or_else(|| Arc::new(ScriptedCodingProvider::new(case, args.variant)));
                runtime
                    .executor_builder_for(provider, workspace)?
                    .approval(AllowAllApproval)
                    .build()
            },
            context,
        )
        .await;
    owner.abort();
    let _ = owner.await;
    let report = outcome?;
    println!("{}", serde_json::to_string_pretty(&report.summary)?);
    if report.cancelled {
        std::process::exit(130);
    }
    if report.summary.passed != report.summary.selected {
        std::process::exit(1);
    }
    Ok(())
}
