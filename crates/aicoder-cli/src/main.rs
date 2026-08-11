use std::{
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use aicoder_core::{
    Agent, AgentConfig, AgentEventHandler, AgentLoop, AgentLoopConfig, ChatClient,
    SessionSelection,
    events::{
        AgentCompletedEvent, ContentChunkEvent, ContentEndedEvent, ContentStartedEvent,
        ReasoningChunkEvent, ReasoningEndedEvent, ReasoningStartedEvent, ToolCallEndedEvent,
        ToolExecutionEndedEvent,
    },
    session::{JsonlSessionRepository, SessionRepository},
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

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List sessions belonging to the selected workspace.
    Sessions,
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
        tokio::task::spawn_blocking(move || {
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
        })
        .await
        .context("Approval prompt task failed")?
    }
}

impl AgentEventHandler for ConsoleEvents {
    fn on_agent_completed(&self, _event: AgentCompletedEvent) {
        // println!("[Finish] Usage {:?}", _event.usage)
    }

    fn on_tool_call_ended(&self, event: ToolCallEndedEvent) {
        println!("[Tool]{:?}", event.tool_call);
    }

    fn on_tool_execution_ended(&self, event: ToolExecutionEndedEvent) {
        println!("[ToolExec]{:?}", event.name);
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
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
    let client = ChatClient::from_env(&model)?;
    let builder = AgentLoop::builder(client)
        .workspace(&cli.workspace)
        .config(AgentLoopConfig::default());
    let agent_loop = if cli.yes {
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
        temperature: Some(0.7),
        top_p: Some(1.0),
        max_tokens: Some(2048),
        seed: None,
        stop: None,
        response_format: None,
    };
    let agent = Agent::new(agent_loop, agent_config);
    let handler = Arc::new(ConsoleEvents);
    let result = match &repository {
        None => agent.run(cli.prompt, handler).await?,
        Some(repository) => {
            let selection = if cli.continue_session {
                SessionSelection::ContinueMostRecent
            } else if let Some(id) = cli.session {
                SessionSelection::Existing(id)
            } else {
                SessionSelection::New
            };
            agent
                .run_with_session(repository, selection, cli.prompt, handler)
                .await?
        }
    };
    if let Some(session) = &result.session {
        eprintln!("Session: {}", session.id);
    }
    println!("Usage :{:?}", result.loop_result.usage);
    Ok(())
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
