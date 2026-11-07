mod packages;
use aicoder_runtime::session::JsonlSessionRepository;
use std::{
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use aicoder_core::{
    Agent, AgentEventHandler, SessionSelection,
    events::{
        AgentCompletedEvent, ContentChunkEvent, ContentEndedEvent, ContentStartedEvent,
        ContextCompactionCompletedEvent, ReasoningChunkEvent, ReasoningEndedEvent,
        ReasoningStartedEvent, ToolCallEndedEvent, ToolExecutionEndedEvent,
    },
    session::{SessionEntryData, SessionRepository},
    tools::{
        AllowAllApproval, ApprovalDecision, ApprovalHandler, ApprovalRequest, ApprovalSessions,
    },
};
use aicoder_runtime::resources::ResourceSettings;
use aicoder_runtime::{
    ConfigDiscovery, ContextSettings, ExecutionSettings, ModelSettings, Runtime, RuntimeOverrides,
    Setting, Settings, environment_file,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
mod execution;
mod exports;
mod input;
mod rpc;

#[derive(Debug, Parser)]
#[command(name = "aicoder", about = "A small tool-using coding agent")]
struct Cli {
    /// User prompt sent to the model.
    #[arg(short, long, default_value = "你是谁")]
    prompt: String,

    /// Prompt as one quoted positional argument.
    #[arg(index = 1, conflicts_with = "prompt")]
    input: Option<String>,
    /// Read the prompt from stdin, bounded to 1 MiB.
    #[arg(long,conflicts_with_all=["input","prompt","repl"])]
    stdin: bool,
    /// Emit only the final assistant text on stdout.
    #[arg(long = "print", conflicts_with = "json")]
    print: bool,
    /// Emit complete event-v1 JSONL on stdout; diagnostics stay on stderr.
    #[arg(long)]
    json: bool,
    /// Read sequential conversation turns; --no-session uses an in-memory session.
    #[arg(long, conflicts_with = "stdin")]
    repl: bool,
    /// Serve versioned local JSONL requests on stdin/stdout.
    #[arg(long, conflicts_with_all = ["input", "prompt", "stdin", "print", "json", "repl"])]
    rpc: bool,
    /// Capture a local workspace image for the initial turn (repeat up to four).
    #[arg(long = "image", value_name = "FILE", conflicts_with = "rpc")]
    images: Vec<PathBuf>,
    /// Automatically approve mutating tools and commands.
    #[arg(long)]
    yes: bool,

    /// Workspace available to file and command tools.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    /// Continue the most recently updated session for this workspace.
    #[arg(short = 'c', long = "continue", conflicts_with_all = ["session", "no_session"])]
    continue_session: bool,

    /// Continue a specific session by its full ID.
    #[arg(long, value_name = "ID", conflicts_with_all = ["continue_session", "no_session"])]
    session: Option<String>,

    /// Run without persistent session or output-artifact storage.
    #[arg(long, conflicts_with_all = ["continue_session", "session"])]
    no_session: bool,

    /// Enable deterministic context pruning for a model with this total context window.
    #[arg(long, value_name = "TOKENS")]
    context_window: Option<usize>,

    /// Maximum model rounds in one turn.
    #[arg(long)]
    max_rounds: Option<usize>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    reasoning_effort: Option<aicoder_runtime::ReasoningOverride>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    max_output_tokens: Option<u32>,
    #[arg(long, conflicts_with = "no_config")]
    config: Option<PathBuf>,
    #[arg(long, conflicts_with = "no_config")]
    user_config: Option<PathBuf>,
    #[arg(long)]
    no_config: bool,
    /// Load an explicitly trusted dotenv file without mutating process variables.
    #[arg(long)]
    env_file: Option<PathBuf>,
    /// Disable all instruction, skill and template discovery.
    #[arg(long)]
    no_resources: bool,
    /// Enable bounded JavaScript tool composition (requires the code-mode build feature).
    #[arg(long)]
    code_mode: bool,
    /// Offer only Code Mode to the model; leaf tools retain their host policies.
    #[arg(long, conflicts_with = "code_mode")]
    code_mode_only: bool,
    /// Explicit trusted host stdio launch file; never discovered from project settings.
    #[arg(long)]
    extension_config: Option<PathBuf>,
    /// Start executable entries from the currently selected, explicitly trusted packages.
    #[arg(long, conflicts_with = "no_packages")]
    activate_packages: bool,
    /// Explicit host program profiles for package dependencies; never project-discovered.
    #[arg(long, requires = "activate_packages")]
    package_programs: Option<PathBuf>,
    /// Disable installed package selection for this invocation.
    #[arg(long, conflicts_with = "package_lock")]
    no_packages: bool,
    /// Select exact installed/trusted pins from an explicit public lock document.
    #[arg(long)]
    package_lock: Option<PathBuf>,
    #[arg(long)]
    no_parent_context: bool,
    #[arg(long)]
    no_skills: bool,
    #[arg(long)]
    no_templates: bool,
    /// Maximum total tool invocations in one turn.
    #[arg(long)]
    max_tool_calls: Option<usize>,
    /// Total token budget, using estimates when usage is unavailable.
    #[arg(long)]
    token_budget: Option<u64>,
    /// Execution time budget in milliseconds, including approvals.
    #[arg(long)]
    time_budget_ms: Option<u64>,
    /// Maximum cumulative generated payload bytes.
    #[arg(long)]
    output_budget_bytes: Option<usize>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage trusted local resource bundles without making model requests.
    Packages {
        #[command(subcommand)]
        command: packages::PackageCommand,
    },
    /// Print effective settings and their sources without resolving credentials or making requests.
    Config,
    /// List resource sources and fingerprints without exposing document bodies.
    Resources,
    /// Inspect a template or explicit /skill expansion without making a model request.
    ExpandPrompt {
        input: String,
    },
    /// List sessions belonging to the selected workspace.
    Sessions,
    /// Show every branch and entry; --json emits metadata and entries for other hosts.
    SessionTree {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Page branch/tree metadata without dumping raw tool payloads.
    SessionBranches {
        id: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read complete canonical archive records with bounded pages.
    SessionArchive {
        id: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Search visible recorded text, with bounded revision-bound pages.
    SearchSession {
        id: String,
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
    },
    RenameSession {
        id: String,
        title: String,
    },
    LabelSession {
        id: String,
        entry: String,
        #[arg(required_unless_present = "remove", conflicts_with = "remove")]
        label: Option<String>,
        #[arg(long)]
        remove: bool,
    },
    BranchSession {
        id: String,
        #[arg(long, conflicts_with = "root")]
        from: Option<String>,
        #[arg(long)]
        root: bool,
        #[arg(long)]
        label: Option<String>,
    },
    RecoverSession {
        id: String,
    },
    /// Copy selected ancestry to a new session; default point is before the latest user.
    ForkSession {
        id: String,
        entry: Option<String>,
        #[arg(long)]
        at: bool,
    },
    /// Clone the complete selected branch into a new session identity.
    CloneSession {
        id: String,
    },
    /// Check compatibility and record a model through the explicitly selected provider.
    SwitchSessionModel {
        id: String,
        model: String,
    },
    /// Validate and record a branch reasoning choice without a model request.
    SetSessionReasoning {
        id: String,
        effort: aicoder_runtime::ReasoningOverride,
    },
    /// Select an entry as the parent of the next turn; other branches remain available.
    CheckoutSession {
        id: String,
        #[arg(required_unless_present = "root", conflicts_with = "root")]
        entry: Option<String>,
        #[arg(long)]
        root: bool,
    },
    /// Upgrade a legacy session, preserving an exact backup before changing its format.
    MigrateSession {
        id: String,
    },
    /// Read a bounded saved-output page without resolving model credentials.
    ReadArtifact {
        id: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long, default_value_t = 8192)]
        max_bytes: usize,
    },
    /// Export all canonical branches to a new private JSON or HTML file.
    ExportSession {
        id: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "json")]
        format: aicoder_runtime::exports::ExportFormat,
        #[command(flatten)]
        limits: ExportBudget,
    },
    /// Explicitly publish a prepared session export to a trusted local share directory.
    ShareSession {
        id: String,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long, default_value = "html")]
        format: aicoder_runtime::exports::ExportFormat,
        #[command(flatten)]
        limits: ExportBudget,
    },
    /// Summarize this session branch without adding a user/coding turn; requires a context window.
    CompactSession {
        id: String,
    },
    /// Permanently delete a session by its full ID.
    DeleteSession {
        /// Session ID shown by `aicoder sessions`.
        id: String,
    },
}

#[derive(Debug, clap::Args)]
struct ExportBudget {
    #[arg(long, default_value_t = 16777216)]
    max_bytes: usize,
    #[arg(long, default_value_t = 10000)]
    max_entries: usize,
    #[arg(long, default_value_t = 134217728)]
    max_source_bytes: u64,
}
struct ConsoleEvents;

struct ConsoleApproval {
    terminal: bool,
}

#[async_trait]
impl ApprovalHandler for ConsoleApproval {
    async fn decide(&self, request: &ApprovalRequest<'_>) -> Result<ApprovalDecision> {
        if self.terminal && !std::io::IsTerminal::is_terminal(&io::stdin()) {
            return Ok(ApprovalDecision::Deny);
        }
        let invocation = request.invocation.clone();
        let session = request.session.clone();
        let (sent, received) = tokio::sync::oneshot::channel();
        let terminal = self.terminal;
        std::thread::Builder::new()
            .name("aicoder-approval".into())
            .spawn(move || {
                if sent.is_closed() {
                    return;
                }
                let result = (|| -> Result<ApprovalDecision> {
                    if let Some(preview) = &invocation.preview {
                        eprintln!(
                            "\n{}\n{}",
                            invocation.name,
                            aicoder_runtime::tools::display_preview(preview)
                        );
                    } else {
                        eprintln!(
                            "\n工具 {} 将以当前用户权限执行（不是沙箱）\n参数: {}",
                            invocation.name,
                            serde_json::to_string_pretty(&invocation.arguments)?
                        );
                    }
                    if let Some(session) = &session {
                        eprintln!(
                            "授权范围: 会话 {:?} / 工作区 {:?} / 工具 {} 的所有参数",
                            session.session_id, session.workspace, invocation.name
                        );
                        eprint!("允许执行? [y 单次 / s 本会话此工具 / N 拒绝] ");
                    } else {
                        eprint!("允许执行? [y/N] ");
                    }
                    io::stderr().flush()?;

                    let Some(input) = input::approval_line(terminal, &sent)? else {
                        return Ok(ApprovalDecision::Deny);
                    };
                    Ok(match input.trim().to_ascii_lowercase().as_str() {
                        "y" | "yes" => ApprovalDecision::AllowOnce,
                        "s" if session.is_some() => ApprovalDecision::AllowSession,
                        _ => ApprovalDecision::Deny,
                    })
                })();
                let _ = sent.send(result);
            })
            .context("Failed to start approval reader")?;
        received.await.context("Approval prompt closed")?
    }
}

impl AgentEventHandler for ConsoleEvents {
    fn on_message_finalized(&self, event: aicoder_core::events::MessageFinalizedEvent) {
        if event.changed
            && matches!(
                event.message.role,
                aicoder_core::model::MessageRole::User
                    | aicoder_core::model::MessageRole::Tool
                    | aicoder_core::model::MessageRole::Custom
            )
            && event.custom.as_ref().is_none_or(|m| m.display)
        {
            let text = event
                .message
                .tool_result()
                .map_or_else(|| event.message.text_content(), |(_, v, _)| v.to_string());
            println!("\n[Finalized {:?} message]\n{text}", event.message.role);
        }
    }

    fn on_assistant_message_finalized(
        &self,
        event: aicoder_core::events::AssistantMessageFinalizedEvent,
    ) {
        if event.changed {
            println!("\n[Finalized assistant]\n{}", event.message.text_content());
        }
    }

    fn on_agent_completed(&self, _event: AgentCompletedEvent) {
        // println!("[Finish] ModelUsage {:?}", _event.usage)
    }

    fn on_tool_call_ended(&self, event: ToolCallEndedEvent) {
        println!("[Tool]{:?}", event.tool_call);
    }

    fn on_tool_execution_ended(&self, event: ToolExecutionEndedEvent) {
        println!("[ToolExec]{:?}", event.name);
        for artifact in event.artifacts {
            eprintln!(
                "[Artifact] {}: {} bytes, complete={}",
                artifact.id, artifact.bytes, artifact.complete
            );
        }
    }

    fn on_context_compaction_completed(&self, event: ContextCompactionCompletedEvent) {
        eprintln!(
            "[Context] {} -> {} estimated tokens, removed {} messages",
            event.estimated_tokens_before, event.estimated_tokens_after, event.removed_messages
        );
    }

    fn on_reasoning_started(&self, _event: ReasoningStartedEvent) {
        print!("[Reasoning]:");
        flush_stdout();
    }

    fn on_reasoning_chunk(&self, event: ReasoningChunkEvent) {
        print!("{}", event.delta);
        flush_stdout();
    }

    fn on_reasoning_ended(&self, _event: ReasoningEndedEvent) {
        println!();
    }

    fn on_content_started(&self, _event: ContentStartedEvent) {
        println!();
        print!("[Content]:");
        flush_stdout();
    }

    fn on_content_chunk(&self, event: ContentChunkEvent) {
        print!("{}", event.delta);
        flush_stdout();
    }

    fn on_content_ended(&self, _event: ContentEndedEvent) {
        println!();
    }
}

fn flush_stdout() {
    let _ = std::io::stdout().flush();
}

async fn guarded_navigation(
    runtime: &Runtime,
    repository: &JsonlSessionRepository,
    id: &str,
    action: aicoder_core::session::ArchiveNavigation,
) -> Result<aicoder_core::session::RunSession> {
    let agent = Agent::new(
        runtime
            .executor_builder(Arc::new(runtime.session_client()?))?
            .build()?,
        runtime.agent_config(),
    )
    .following_session_models()
    .following_session_reasoning();
    let context = aicoder_core::TurnExecutionContext::new();
    let outcome = execution::cancellable(
        context.clone(),
        agent.navigate_session(repository, id, action, context),
    )
    .await??;
    if !outcome.report.delivery.failures.is_empty() {
        eprintln!("Session change committed; lifecycle observations were incomplete.");
    }
    Ok(outcome.session)
}
async fn fork_cli(
    runtime: &Runtime,
    repository: &JsonlSessionRepository,
    id: &str,
    point: aicoder_core::session::ForkPoint,
) -> Result<()> {
    let agent = Agent::new(
        runtime
            .executor_builder(Arc::new(runtime.session_client()?))?
            .build()?,
        runtime.agent_config(),
    )
    .following_session_models()
    .following_session_reasoning();
    let context = aicoder_core::TurnExecutionContext::new();
    let output = execution::cancellable(
        context.clone(),
        agent.fork_session(
            repository,
            id,
            point,
            aicoder_core::session::SessionForkLimits::default(),
            context,
        ),
    )
    .await??;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"session":output.session.metadata(),"editor":output.editor,"provenance":output.provenance,"lifecycle":output.reports})
        )?
    );
    Ok(())
}
async fn handle_session_command(
    cli: &Cli,
    runtime: &Runtime,
    repository: &JsonlSessionRepository,
) -> Result<bool> {
    match &cli.command {
        Some(Command::SessionBranches { id, limit, cursor }) => {
            let navigator =
                aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?;
            let cursor = cursor.as_deref().map(serde_json::from_str).transpose()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&navigator.tree(id, *limit, cursor.as_ref())?)?
            );
            Ok(true)
        }
        Some(Command::SearchSession {
            id,
            query,
            limit,
            cursor,
        }) => {
            let navigator =
                aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?;
            let cursor = cursor.as_deref().map(serde_json::from_str).transpose()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&navigator.search(
                    id,
                    query,
                    *limit,
                    cursor.as_ref()
                )?)?
            );
            Ok(true)
        }
        Some(Command::SessionArchive { id, limit, cursor }) => {
            let nav = aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?;
            let cursor = cursor.as_deref().map(serde_json::from_str).transpose()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&nav.archive(id, *limit, cursor.as_ref())?)?
            );
            Ok(true)
        }
        Some(Command::RenameSession { id, title }) => {
            let session = guarded_navigation(
                runtime,
                repository,
                id,
                aicoder_core::session::ArchiveNavigation::Rename {
                    title: title.clone(),
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(session.metadata())?);
            Ok(true)
        }
        Some(Command::LabelSession {
            id, entry, label, ..
        }) => {
            let session = guarded_navigation(
                runtime,
                repository,
                id,
                aicoder_core::session::ArchiveNavigation::Label {
                    entry_id: entry.clone(),
                    label: label.clone(),
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(session.metadata())?);
            Ok(true)
        }
        Some(Command::BranchSession {
            id,
            from,
            root,
            label,
        }) => {
            let source = if *root {
                aicoder_core::session::BranchSource::Root
            } else if let Some(from) = from {
                aicoder_core::session::BranchSource::Entry(from.clone())
            } else {
                aicoder_core::session::BranchSource::Current
            };
            let session = guarded_navigation(
                runtime,
                repository,
                id,
                aicoder_core::session::ArchiveNavigation::Branch {
                    source,
                    name: label.clone(),
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(session.metadata())?);
            Ok(true)
        }
        Some(Command::ForkSession { id, entry, at }) => {
            let point = match (entry, at) {
                (Some(id), true) => aicoder_core::session::ForkPoint::At(id.clone()),
                (Some(id), false) => aicoder_core::session::ForkPoint::Before(id.clone()),
                (None, false) => aicoder_core::session::ForkPoint::BeforeLastUser,
                (None, true) => anyhow::bail!("--at requires an entry or label"),
            };
            fork_cli(runtime, repository, id, point).await?;
            Ok(true)
        }
        Some(Command::CloneSession { id }) => {
            fork_cli(
                runtime,
                repository,
                id,
                aicoder_core::session::ForkPoint::Current,
            )
            .await?;
            Ok(true)
        }
        Some(Command::RecoverSession { id }) => {
            let session = guarded_navigation(
                runtime,
                repository,
                id,
                aicoder_core::session::ArchiveNavigation::Recover,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(session.metadata())?);
            Ok(true)
        }
        Some(Command::Sessions) => {
            let sessions = repository.list(&cli.workspace)?;
            if sessions.is_empty() {
                println!("当前 workspace 没有 session");
            } else {
                for session in sessions {
                    println!(
                        "{}\t{} messages\t{}",
                        session.id,
                        session.message_count,
                        session.title.as_deref().unwrap_or("(无标题)")
                    );
                }
            }
            Ok(true)
        }
        Some(Command::SessionTree { id, json }) => {
            let session = aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?
                .open(id)?;
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"metadata":session.metadata(),"entries":session.entries()})
                    )?
                );
            } else {
                for entry in session.entries() {
                    let kind = match &entry.data {
                        SessionEntryData::Message { message } => format!("{:?}", message.role),
                        SessionEntryData::CustomMessage { .. } => "CustomMessage".into(),
                        SessionEntryData::NativeContext { .. } => "NativeContext".into(),
                        SessionEntryData::ModelChanged { provider, model } => {
                            format!("model {provider}/{model}")
                        }
                        SessionEntryData::Compaction { strategy, .. } => {
                            format!("compaction {strategy}")
                        }
                        SessionEntryData::Extension { namespace, .. } => {
                            format!("extension {namespace}")
                        }
                        SessionEntryData::TurnStart { .. } => "turn_start".into(),
                        SessionEntryData::TurnEnd { record, .. } => {
                            if record.stop.is_some() {
                                "turn_interrupted".into()
                            } else {
                                "turn_completed".into()
                            }
                        }
                    };
                    let selected =
                        if session.metadata().leaf_id.as_deref() == Some(entry.id.as_str()) {
                            "*"
                        } else {
                            " "
                        };
                    println!(
                        "{selected} {} <- {}\t{kind}",
                        entry.id,
                        entry.parent_id.as_deref().unwrap_or("root")
                    );
                }
            }
            Ok(true)
        }
        Some(Command::CheckoutSession { id, entry, .. }) => {
            guarded_navigation(
                runtime,
                repository,
                id,
                aicoder_core::session::ArchiveNavigation::Checkout {
                    target: entry.clone(),
                },
            )
            .await?;
            println!(
                "Session {id}: selected {}",
                entry.as_deref().unwrap_or("root")
            );
            Ok(true)
        }
        Some(Command::MigrateSession { id }) => {
            let mut session =
                aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?
                    .open(id)?;
            if let Some(backup) = repository.migrate(&mut session)? {
                println!(
                    "Session {id}: v{}, backup {}",
                    session.metadata().version,
                    backup.display()
                );
            } else {
                println!("Session {id}: already v{}", session.metadata().version);
            }
            Ok(true)
        }
        Some(Command::DeleteSession { id }) => {
            aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?.info(id)?;
            repository.delete(id)?;
            println!("已删除 session {id}");
            Ok(true)
        }
        Some(
            Command::Config
            | Command::Packages { .. }
            | Command::Resources
            | Command::ExpandPrompt { .. }
            | Command::CompactSession { .. }
            | Command::ExportSession { .. }
            | Command::ShareSession { .. }
            | Command::ReadArtifact { .. }
            | Command::SwitchSessionModel { .. }
            | Command::SetSessionReasoning { .. },
        )
        | None => Ok(false),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match application().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            if error.downcast_ref::<input::Interrupted>().is_some() {
                std::process::ExitCode::from(130)
            } else {
                std::process::ExitCode::FAILURE
            }
        }
    }
}
async fn application() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .with_writer(io::stderr)
        .init();
    let matches = Cli::command().get_matches();
    let explicit_prompt =
        matches.value_source("prompt") == Some(clap::parser::ValueSource::CommandLine);
    let cli = Cli::from_arg_matches(&matches)?;
    anyhow::ensure!(
        cli.command.is_none()
            || !(cli.repl
                || cli.stdin
                || cli.print
                || cli.json
                || cli.rpc
                || cli.extension_config.is_some()
                || cli.activate_packages
                || cli.input.is_some()
                || !cli.images.is_empty()),
        "execution input/output flags cannot be combined with metadata commands"
    );

    let runtime = Runtime::load_with_environment(
        &cli.workspace,
        RuntimeOverrides {
            settings: Settings {
                provider_profile: Setting::from_option(cli.profile.clone()),
                model: Setting::from_option(cli.model.clone()),
                options: ModelSettings {
                    reasoning_effort: cli
                        .reasoning_effort
                        .map(|v| v.setting())
                        .unwrap_or(Setting::Inherit),
                    max_output_tokens: Setting::from_option(cli.max_output_tokens),
                    ..Default::default()
                },
                execution: ExecutionSettings {
                    max_rounds: Setting::from_option(cli.max_rounds),
                    max_tool_calls: Setting::from_option(cli.max_tool_calls),
                    max_tokens: Setting::from_option(cli.token_budget),
                    max_duration_ms: Setting::from_option(cli.time_budget_ms),
                    max_output_bytes: Setting::from_option(cli.output_budget_bytes),
                    ..Default::default()
                },
                packages: aicoder_runtime::packages::PackageSettings {
                    enabled: if cli.no_packages {
                        Setting::Value(false)
                    } else {
                        Setting::Inherit
                    },
                    ..Default::default()
                },
                code_mode: aicoder_runtime::code_mode::CodeModeSettings {
                    only: if cli.code_mode_only {
                        Setting::Value(true)
                    } else if cli.code_mode {
                        Setting::Value(false)
                    } else {
                        Setting::Inherit
                    },
                    enabled: if cli.code_mode || cli.code_mode_only {
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
                    parents: if cli.no_parent_context {
                        Setting::Value(false)
                    } else {
                        Setting::Inherit
                    },
                    skills: if cli.no_skills {
                        Setting::Value(false)
                    } else {
                        Setting::Inherit
                    },
                    prompts: if cli.no_templates {
                        Setting::Value(false)
                    } else {
                        Setting::Inherit
                    },
                    ..Default::default()
                },
                context: ContextSettings {
                    window_tokens: Setting::from_option(cli.context_window),
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
            user_file: cli.user_config.clone(),
            project_file: cli.config.clone(),
        },
        environment_file(cli.env_file.as_deref())?,
    )?;
    if let Some(Command::Packages { command }) = &cli.command {
        return packages::execute(&runtime, command).await;
    }
    let runtime = if let Some(path) = &cli.package_lock {
        runtime.with_package_state(&aicoder_runtime::packages::PackageSessionState::read(path)?)?
    } else {
        runtime
    };
    if matches!(cli.command, Some(Command::Config)) {
        println!("{}", serde_json::to_string_pretty(runtime.config())?);
        return Ok(());
    }
    if matches!(cli.command, Some(Command::Resources)) {
        println!(
            "{}",
            serde_json::to_string_pretty(runtime.resources()?.manifest())?
        );
        return Ok(());
    }
    if let Some(Command::ExpandPrompt { input }) = &cli.command {
        println!(
            "{}",
            serde_json::to_string_pretty(&runtime.prepare_prompt(input)?)?
        );
        return Ok(());
    }
    if let Some(Command::ReadArtifact {
        id,
        offset,
        max_bytes,
    }) = &cli.command
    {
        use aicoder_core::tools::ExecutableTool;
        let store = runtime
            .artifact_store_for(&runtime.config().workspace)?
            .ok_or_else(|| anyhow::anyhow!("Artifact storage is disabled"))?;
        let context = aicoder_core::tools::ToolContext::new(
            &runtime.config().workspace,
            std::time::Duration::from_secs(30),
            64 * 1024,
        )?
        .with_artifact_store(store);
        let page = aicoder_core::artifacts::ReadArtifactTool
            .execute(
                &context,
                serde_json::json!({"id":id,"offset":offset,"max_bytes":max_bytes}),
            )
            .await
            .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?;
        println!("{}", serde_json::to_string_pretty(&page.output)?);
        return Ok(());
    }
    if let Some(Command::SetSessionReasoning { id, effort }) = &cli.command {
        let repository = runtime.sessions()?;
        let agent = Agent::new(
            runtime
                .executor_builder(Arc::new(runtime.session_client()?))?
                .build()?,
            runtime.agent_config(),
        )
        .following_session_models();
        let context = aicoder_core::TurnExecutionContext::new();
        let result = execution::cancellable(
            context.clone(),
            agent.switch_session_reasoning(&repository, id, effort.0, context.clone()),
        )
        .await?;
        if context.is_cancelled() {
            return Err(input::Interrupted.into());
        }
        println!("{}", serde_json::to_string_pretty(&result?)?);
        return Ok(());
    }
    if let Some(Command::SwitchSessionModel { id, model }) = &cli.command {
        let repository = runtime.sessions()?;
        let agent = Agent::new(
            runtime
                .executor_builder(Arc::new(runtime.session_client()?))?
                .build()?,
            runtime.agent_config(),
        );
        let agent = if runtime.follows_session_reasoning() {
            agent.following_session_reasoning()
        } else {
            agent
        };
        let context = aicoder_core::TurnExecutionContext::new();
        let result = execution::cancellable(
            context.clone(),
            agent.switch_session_model(&repository, id, model, context.clone()),
        )
        .await?;
        if context.is_cancelled() {
            return Err(input::Interrupted.into());
        }
        let report = result?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if let Some(Command::CompactSession { id }) = &cli.command {
        let repository = runtime.sessions()?;
        let agent = runtime.summary_agent(Arc::new(runtime.session_client()?))?;
        let context = aicoder_core::TurnExecutionContext::new();
        let report = execution::cancellable(
            context.clone(),
            agent.compact_session(&repository, id, context.clone()),
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        if context.is_cancelled() {
            return Err(input::Interrupted.into());
        }
        if !report.succeeded() {
            if report.checkpoint_entry_id.is_some() {
                anyhow::bail!(
                    "Compaction checkpoint committed; later work was incomplete. Inspect the report before another request"
                );
            }
            anyhow::bail!(
                "Manual compaction did not commit a summary; inspect the report before another request"
            );
        }
        return Ok(());
    }
    if exports::handle(&cli, &runtime).await? {
        return Ok(());
    }
    if cli.command.is_some() {
        let repository = runtime.sessions()?;
        if handle_session_command(&cli, &runtime, &repository).await? {
            return Ok(());
        }
    }
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
        let active = execution::cancellable(
            context.clone(),
            runtime.activate_host_extensions(&manifest, programs.as_ref(), context.clone()),
        )
        .await??;
        Some(active)
    } else {
        None
    };
    let runtime = if let Some(active) = &activation {
        runtime.with_extensions(active.extensions())?
    } else {
        runtime
    };
    let outcome = if cli.rpc {
        rpc::run(&cli, &runtime).await
    } else {
        execution::run(&cli, explicit_prompt, &runtime).await
    };
    match activation {
        Some(active) => active.finish(outcome).await,
        None => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_accepts_prompt() {
        let cli = Cli::try_parse_from(["aicoder", "--prompt", "检查当前项目"]).unwrap();
        assert_eq!(cli.prompt, "检查当前项目");
        assert!(!cli.yes);
        assert_eq!(cli.workspace, PathBuf::from("."));
        assert!(!cli.continue_session);
        assert!(cli.session.is_none());
        assert!(!cli.no_session);
    }

    #[test]
    fn cli_accepts_context_window() {
        let cli = Cli::try_parse_from([
            "aicoder",
            "--prompt",
            "检查当前项目",
            "--context-window",
            "65536",
        ])
        .unwrap();

        assert_eq!(cli.context_window, Some(65_536));
    }

    #[test]
    fn cli_keeps_default_prompt() {
        let cli = Cli::try_parse_from(["aicoder"]).unwrap();
        assert_eq!(cli.prompt, "你是谁");
    }

    #[test]
    fn cli_accepts_session_selection_modes() {
        let continued = Cli::try_parse_from(["aicoder", "--continue"]).unwrap();
        assert!(continued.continue_session);

        let selected = Cli::try_parse_from([
            "aicoder",
            "--session",
            "58ed33e6-26fc-4688-81ad-909d63af5ad7",
        ])
        .unwrap();
        assert_eq!(
            selected.session.as_deref(),
            Some("58ed33e6-26fc-4688-81ad-909d63af5ad7")
        );

        assert!(Cli::try_parse_from(["aicoder", "--continue", "--no-session"]).is_err());
    }

    #[test]
    fn cli_accepts_session_management_commands() {
        let listed = Cli::try_parse_from(["aicoder", "sessions"]).unwrap();
        assert!(matches!(listed.command, Some(Command::Sessions)));

        let deleted = Cli::try_parse_from([
            "aicoder",
            "delete-session",
            "58ed33e6-26fc-4688-81ad-909d63af5ad7",
        ])
        .unwrap();
        assert!(matches!(
            deleted.command,
            Some(Command::DeleteSession { .. })
        ));
    }
}
