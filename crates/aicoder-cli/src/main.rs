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
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "aicoder", about = "A small tool-using coding agent")]
struct Cli {
    /// User prompt sent to the model.
    #[arg(short, long, default_value = "你是谁")]
    prompt: String,

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
    /// Check compatibility and record a model through the explicitly selected provider.
    SwitchSessionModel {
        id: String,
        model: String,
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

struct ConsoleEvents;

struct ConsoleApproval;

#[async_trait]
impl ApprovalHandler for ConsoleApproval {
    async fn decide(&self, request: &ApprovalRequest<'_>) -> Result<ApprovalDecision> {
        let invocation = request.invocation.clone();
        let session = request.session.clone();
        let (sent, received) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("aicoder-approval".into())
            .spawn(move || {
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

                    let mut input = String::new();
                    io::stdin().read_line(&mut input)?;
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

fn handle_session_command(cli: &Cli, repository: &JsonlSessionRepository) -> Result<bool> {
    match &cli.command {
        Some(Command::SessionBranches { id, limit, cursor }) => {
            let navigator =
                aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?;
            let cursor = cursor.as_deref().map(serde_json::from_str).transpose()?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &navigator.open(id)?.tree_page(*limit, cursor.as_ref())?
                )?
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
                serde_json::to_string_pretty(&navigator.open(id)?.search_messages(
                    query,
                    *limit,
                    cursor.as_ref()
                )?)?
            );
            Ok(true)
        }
        Some(Command::RenameSession { id, title }) => {
            let session = aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?
                .rename(id, title)?;
            println!("{}", serde_json::to_string_pretty(session.metadata())?);
            Ok(true)
        }
        Some(Command::LabelSession {
            id, entry, label, ..
        }) => {
            let session = aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?
                .label(id, entry, label.as_deref())?;
            println!("{}", serde_json::to_string_pretty(session.metadata())?);
            Ok(true)
        }
        Some(Command::BranchSession {
            id,
            from,
            root,
            label,
        }) => {
            let navigator =
                aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?;
            let source = if *root {
                None
            } else {
                match from {
                    Some(entry) => Some(entry.clone()),
                    None => navigator.open(id)?.metadata().leaf_id.clone(),
                }
            };
            let session = navigator.branch(id, source.as_deref(), label.as_deref())?;
            println!("{}", serde_json::to_string_pretty(session.metadata())?);
            Ok(true)
        }
        Some(Command::RecoverSession { id }) => {
            let session = aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?
                .recover(id)?;
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
            aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?
                .checkout(id, entry.as_deref())?;
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
            aicoder_core::session::SessionNavigator::new(repository, &cli.workspace)?.open(id)?;
            repository.delete(id)?;
            println!("已删除 session {id}");
            Ok(true)
        }
        Some(
            Command::Config
            | Command::Resources
            | Command::ExpandPrompt { .. }
            | Command::CompactSession { .. }
            | Command::ReadArtifact { .. }
            | Command::SwitchSessionModel { .. },
        )
        | None => Ok(false),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();
    let cli = Cli::parse();

    let runtime = Runtime::load_with_environment(
        &cli.workspace,
        RuntimeOverrides {
            settings: Settings {
                provider_profile: Setting::from_option(cli.profile.clone()),
                model: Setting::from_option(cli.model.clone()),
                options: ModelSettings {
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
    if let Some(Command::SwitchSessionModel { id, model }) = &cli.command {
        let repository = runtime.sessions()?;
        let agent = Agent::new(
            runtime
                .executor_builder(Arc::new(runtime.session_client()?))?
                .build()?,
            runtime.agent_config(),
        );
        let report = agent
            .switch_session_model(
                &repository,
                id,
                model,
                aicoder_core::TurnExecutionContext::new(),
            )
            .await?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if let Some(Command::CompactSession { id }) = &cli.command {
        let repository = runtime.sessions()?;
        let agent = runtime.summary_agent(Arc::new(runtime.session_client()?))?;
        let report = agent
            .compact_session(&repository, id, aicoder_core::TurnExecutionContext::new())
            .await;
        println!("{}", serde_json::to_string_pretty(&report)?);
        if !report.succeeded() {
            anyhow::bail!(
                "Manual compaction did not commit a summary; inspect the report before retrying"
            );
        }
        return Ok(());
    }
    if cli.command.is_some() {
        let repository = runtime.sessions()?;
        if handle_session_command(&cli, &repository)? {
            return Ok(());
        }
    }
    let repository = if cli.no_session {
        None
    } else {
        Some(runtime.sessions()?)
    };
    let builder = runtime.executor_builder(Arc::new(runtime.session_client()?))?;
    let executor = if cli.yes {
        builder.approval(AllowAllApproval).build()?
    } else {
        builder.approval(ConsoleApproval).build()?
    };
    let agent = Agent::new(executor, runtime.agent_config());
    let agent = if cli.model.is_none() && cli.profile.is_none() {
        agent.following_session_models()
    } else {
        agent
    };
    let prompt = runtime.prepare_prompt(&cli.prompt)?.text;
    let handler = Arc::new(ConsoleEvents);
    let outcome = match &repository {
        None => {
            agent
                .run_outcome(prompt, handler, aicoder_core::TurnExecutionContext::new())
                .await
        }
        Some(repository) => {
            let selection = if cli.continue_session {
                SessionSelection::ContinueMostRecent
            } else if let Some(id) = cli.session {
                SessionSelection::Existing(id)
            } else {
                SessionSelection::New
            };
            agent
                .run_with_session_outcome(
                    repository,
                    selection,
                    prompt,
                    handler,
                    aicoder_core::TurnExecutionContext::new()
                        .with_approval_sessions(ApprovalSessions::default()),
                )
                .await
        }
    };
    if let Some(session) = outcome.session() {
        eprintln!("Session: {}", session.id);
    }
    let progress = outcome.progress();
    if progress.event_delivery.has_failures() {
        eprintln!(
            "Live event delivery was incomplete: {}",
            serde_json::to_string(&progress.event_delivery)?
        );
    }
    let count = |value: Option<u64>| {
        value
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unknown".into())
    };
    println!(
        "Tokens: input={}, output={}, total={}",
        count(progress.usage.input_tokens),
        count(progress.usage.output_tokens),
        count(progress.usage.total_tokens)
    );
    if let Some(limit) = cli.token_budget {
        println!(
            "Token budget: {}/{limit}{}",
            progress.tokens.used,
            if progress.tokens.estimated {
                " (estimated)"
            } else {
                ""
            }
        );
    }
    outcome.into_result().map(|_| ())
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
