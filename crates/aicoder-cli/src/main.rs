use aicoder_providers::ChatClient;
use std::{
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use aicoder_core::{
    Agent, AgentConfig, AgentEventHandler, ContextWindowConfig, PruningContextCompactor,
    SessionSelection, TurnExecutionConfig, TurnExecutor,
    events::{
        AgentCompletedEvent, ContentChunkEvent, ContentEndedEvent, ContentStartedEvent,
        ContextCompactionCompletedEvent, ReasoningChunkEvent, ReasoningEndedEvent,
        ReasoningStartedEvent, ToolCallEndedEvent, ToolExecutionEndedEvent,
    },
    session::{JsonlSessionRepository, SessionEntryData, SessionRepository},
    tools::{AllowAllApproval, ApprovalHandler, ToolInvocation},
};
use anyhow::{Context, Result, ensure};
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

    /// Run without creating or updating a session.
    #[arg(long, conflicts_with_all = ["continue_session", "session"])]
    no_session: bool,

    /// Enable deterministic context pruning for a model with this total context window.
    #[arg(long, value_name = "TOKENS")]
    context_window: Option<usize>,

    /// Maximum model rounds in one turn.
    #[arg(long, default_value_t = 8)]
    max_rounds: usize,
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
    /// List sessions belonging to the selected workspace.
    Sessions,
    /// Show every branch and entry; --json emits metadata and entries for other hosts.
    SessionTree {
        id: String,
        #[arg(long)]
        json: bool,
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
    MigrateSession { id: String },
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
    async fn approve(&self, invocation: &ToolInvocation) -> Result<bool> {
        let invocation = invocation.clone();
        let (sent, received) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("aicoder-approval".into())
            .spawn(move || {
                let result = (|| -> Result<bool> {
                    eprintln!(
                        "\n工具 {} 将以当前用户权限执行（不是沙箱）\n参数: {}",
                        invocation.name,
                        serde_json::to_string_pretty(&invocation.arguments)?
                    );
                    eprint!("允许执行? [y/N] ");
                    io::stderr().flush()?;

                    let mut input = String::new();
                    io::stdin().read_line(&mut input)?;
                    Ok(matches!(
                        input.trim().to_ascii_lowercase().as_str(),
                        "y" | "yes"
                    ))
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

fn load_dotenv() -> Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => Ok(()),
        Err(error) if error.not_found() => {
            // `cargo run` may be invoked from the workspace root. In that case dotenvy's
            // current-directory search cannot see the CLI crate's own `.env` file.
            let cli_env = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env");
            if cli_env.is_file() {
                dotenvy::from_path(&cli_env)
                    .with_context(|| format!("Failed to load {}", cli_env.display()))?;
            }
            Ok(())
        }
        Err(error) => Err(error).context("Failed to load .env"),
    }
}

fn session_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AICODER_HOME") {
        ensure!(!path.is_empty(), "AICODER_HOME cannot be empty");
        return Ok(PathBuf::from(path).join("sessions"));
    }
    let home = std::env::var_os("HOME").context(
        "Cannot determine session directory: set AICODER_HOME or HOME, or use --no-session",
    )?;
    ensure!(!home.is_empty(), "HOME cannot be empty");
    Ok(PathBuf::from(home).join(".aicoder").join("sessions"))
}

fn handle_session_command(cli: &Cli, repository: &JsonlSessionRepository) -> Result<bool> {
    match &cli.command {
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
            let session = repository.open(id)?;
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
            let mut session = repository.open(id)?;
            repository.checkout(&mut session, entry.as_deref())?;
            println!(
                "Session {id}: selected {}",
                entry.as_deref().unwrap_or("root")
            );
            Ok(true)
        }
        Some(Command::MigrateSession { id }) => {
            let mut session = repository.open(id)?;
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
            repository.delete(id)?;
            println!("已删除 session {id}");
            Ok(true)
        }
        None => Ok(false),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    load_dotenv()?;
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();
    let cli = Cli::parse();

    if cli.command.is_some() {
        let repository = JsonlSessionRepository::new(session_root()?)?;
        if handle_session_command(&cli, &repository)? {
            return Ok(());
        }
    }

    let repository = if cli.no_session {
        None
    } else {
        Some(JsonlSessionRepository::new(session_root()?)?)
    };
    let client = ChatClient::from_env_model(None)?;
    let model = client.model().to_string();
    let builder = TurnExecutor::builder(client)
        .workspace(&cli.workspace)
        .config(TurnExecutionConfig {
            events: Default::default(),
            max_rounds: cli.max_rounds,
            stream: true,
            budget: aicoder_core::RunBudget {
                max_tool_calls: cli.max_tool_calls,
                max_tokens: cli.token_budget,
                max_duration: cli.time_budget_ms.map(std::time::Duration::from_millis),
                max_output_bytes: cli.output_budget_bytes,
            },
        });
    let builder = match cli.context_window {
        Some(max_context_tokens) => {
            builder.context_compactor(PruningContextCompactor::new(ContextWindowConfig {
                max_context_tokens,
                reserved_output_tokens: 4_096,
                preserve_recent_tokens: 8_192.min(max_context_tokens.saturating_sub(4_097)),
            })?)
        }
        None => builder,
    };
    let turn_executor = if cli.yes {
        builder.approval(AllowAllApproval).build()?
    } else {
        builder.approval(ConsoleApproval).build()?
    };
    let agent_config = AgentConfig {
        model,
        system_prompt: Some(
            "You are a helpful coding assistant. Reply in Chinese. Use tools when needed."
                .to_string(),
        ),
        options: aicoder_core::model::ModelOptions {
            max_output_tokens: Some(2048),
            ..Default::default()
        },
    };
    let agent = Agent::new(turn_executor, agent_config);
    let handler = Arc::new(ConsoleEvents);
    let outcome = match &repository {
        None => {
            agent
                .run_outcome(
                    cli.prompt,
                    handler,
                    aicoder_core::TurnExecutionContext::new(),
                )
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
                    cli.prompt,
                    handler,
                    aicoder_core::TurnExecutionContext::new(),
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
