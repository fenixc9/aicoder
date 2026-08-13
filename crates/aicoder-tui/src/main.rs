mod app;
mod runtime;
mod ui;

use std::{
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use aicoder_core::TurnExecutionContext;
use anyhow::{Context, Result, ensure};
use app::{App, AppEvent, Focus};
use clap::Parser;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use runtime::AgentRuntime;
use tokio::sync::mpsc;

#[derive(Debug, Parser)]
#[command(name = "aicoder-tui", about = "Interactive terminal UI for aicoder")]
struct Cli {
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, value_name = "TOKENS")]
    context_window: Option<usize>,

    /// Open an existing session by its full ID.
    #[arg(long, value_name = "ID")]
    session: Option<String>,
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("Failed to enable terminal raw mode")?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error).context("Failed to enter alternate terminal screen");
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    load_dotenv()?;
    let cli = Cli::parse();
    init_logging()?;
    let workspace = cli
        .workspace
        .canonicalize()
        .with_context(|| format!("Failed to resolve workspace {}", cli.workspace.display()))?;
    let model = cli
        .model
        .or_else(|| std::env::var("OPENAI_MODEL").ok())
        .unwrap_or_else(|| "deepseek-v4-flash".into());
    let (sender, receiver) = mpsc::unbounded_channel();
    let runtime = AgentRuntime::new(
        workspace,
        session_root()?,
        model.clone(),
        cli.context_window,
        sender,
    )?;
    let mut app = App::new(runtime.sessions()?);
    if let Some(session_id) = cli.session {
        let session = runtime.open_session(&session_id)?;
        app.selected_session = app
            .sessions
            .iter()
            .position(|candidate| candidate.id == session_id)
            .context("Selected session is missing from the workspace session list")?;
        app.load_session(&session);
    }

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("Failed to initialize terminal")?;
    terminal.clear()?;
    run(&mut terminal, app, runtime, receiver, &model).await
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut app: App,
    runtime: AgentRuntime,
    mut receiver: mpsc::UnboundedReceiver<AppEvent>,
    model: &str,
) -> Result<()> {
    let mut terminal_events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        terminal.draw(|frame| ui::draw(frame, &app, model))?;
        if app.should_quit {
            break;
        }
        tokio::select! {
            _ = tick.tick() => {}
            Some(event) = receiver.recv() => {
                let finished = matches!(event, AppEvent::TurnFinished(_));
                app.apply(event);
                if finished {
                    app.sessions = runtime.sessions().unwrap_or_else(|error| {
                        tracing::error!(error = %format!("{error:#}"), "Failed to refresh sessions");
                        Vec::new()
                    });
                }
            }
            event = terminal_events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        handle_key(&mut app, &runtime, key)?;
                    }
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error).context("Failed to read terminal event"),
                    None => break,
                }
            }
        }
    }
    if app.is_running() {
        app.cancel("TUI exited");
    }
    Ok(())
}

fn handle_key(app: &mut App, runtime: &AgentRuntime, key: KeyEvent) -> Result<()> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if app.is_running() {
            app.cancel("Ctrl-C pressed");
        } else {
            app.should_quit = true;
        }
        return Ok(());
    }
    if app.approval.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => app.resolve_approval(true),
            KeyCode::Char('n') | KeyCode::Char('N') => app.resolve_approval(false),
            KeyCode::Esc => app.cancel("Approval cancelled"),
            _ => {}
        }
        return Ok(());
    }
    if app.confirm_delete {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(session) = app.sessions.get(app.selected_session) {
                    runtime.delete_session(&session.id)?;
                    app.sessions = runtime.sessions()?;
                    app.selected_session = app
                        .selected_session
                        .min(app.sessions.len().saturating_sub(1));
                    app.select_new_session();
                }
                app.confirm_delete = false;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.confirm_delete = false;
            }
            _ => {}
        }
        return Ok(());
    }
    if key.code == KeyCode::Esc && app.is_running() {
        app.cancel("Escape pressed");
        return Ok(());
    }
    match key.code {
        KeyCode::Tab => {
            app.focus = match app.focus {
                Focus::Sessions => Focus::Input,
                Focus::Input => Focus::Sessions,
            };
        }
        KeyCode::PageUp => app.scroll_back = app.scroll_back.saturating_add(8),
        KeyCode::PageDown => app.scroll_back = app.scroll_back.saturating_sub(8),
        _ if app.focus == Focus::Sessions && !app.is_running() => {
            handle_session_key(app, runtime, key)?
        }
        _ if app.focus == Focus::Input && !app.is_running() => handle_input_key(app, runtime, key),
        _ => {}
    }
    Ok(())
}

fn handle_session_key(app: &mut App, runtime: &AgentRuntime, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.sessions.is_empty() {
                app.selected_session = (app.selected_session + 1).min(app.sessions.len() - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.selected_session = app.selected_session.saturating_sub(1);
        }
        KeyCode::Enter => {
            if let Some(session) = app.sessions.get(app.selected_session) {
                app.load_session(&runtime.open_session(&session.id)?);
            }
        }
        KeyCode::Char('n') => app.select_new_session(),
        KeyCode::Char('d') if !app.sessions.is_empty() => app.confirm_delete = true,
        _ => {}
    }
    Ok(())
}

fn handle_input_key(app: &mut App, runtime: &AgentRuntime, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            let prompt = app.input.take();
            if prompt.trim().is_empty() {
                return;
            }
            let selection = app.session_selection.clone();
            let context = TurnExecutionContext::new();
            runtime.start_turn(selection, prompt.clone(), context.clone());
            app.begin_turn(prompt, context);
        }
        KeyCode::Char(character)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.input.insert(character)
        }
        KeyCode::Backspace => app.input.backspace(),
        KeyCode::Delete => app.input.delete(),
        KeyCode::Left => app.input.move_left(),
        KeyCode::Right => app.input.move_right(),
        KeyCode::Home => app.input.move_home(),
        KeyCode::End => app.input.move_end(),
        _ => {}
    }
}

fn load_dotenv() -> Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => Ok(()),
        Err(error) if error.not_found() => {
            // `cargo run` is commonly invoked from the workspace root, where dotenvy's
            // directory search cannot see crate-local files. Prefer the TUI file and keep the
            // existing CLI file as a compatibility fallback for current installations.
            for path in crate_env_paths() {
                if path.is_file() {
                    dotenvy::from_path(&path)
                        .with_context(|| format!("Failed to load {}", path.display()))?;
                    break;
                }
            }
            Ok(())
        }
        Err(error) => Err(error).context("Failed to load .env"),
    }
}

fn crate_env_paths() -> [PathBuf; 2] {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    [
        manifest.join(".env"),
        manifest.join("..").join("aicoder-cli").join(".env"),
    ]
}

fn session_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AICODER_HOME") {
        ensure!(!path.is_empty(), "AICODER_HOME cannot be empty");
        return Ok(PathBuf::from(path).join("sessions"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    ensure!(!home.is_empty(), "HOME cannot be empty");
    Ok(PathBuf::from(home).join(".aicoder").join("sessions"))
}

fn init_logging() -> Result<()> {
    let root = std::env::var_os("AICODER_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".aicoder")))
        .context("Cannot determine log directory")?;
    std::fs::create_dir_all(&root)?;
    let log_path = root.join("tui.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("Failed to open {}", log_path.display()))?;
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_target(false)
        .with_writer(log)
        .try_init()
        .map_err(|error| anyhow::anyhow!("Failed to initialize logging: {error}"))?;
    Ok(())
}

#[allow(dead_code)]
fn _is_inside(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_fallback_prefers_tui_then_existing_cli_file() {
        let paths = crate_env_paths();
        assert_eq!(
            paths[0].file_name().and_then(|name| name.to_str()),
            Some(".env")
        );
        assert_eq!(
            paths[0]
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            Some("aicoder-tui")
        );
        assert_eq!(
            paths[1]
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            Some("aicoder-cli")
        );
    }

    #[test]
    fn cli_accepts_an_existing_session_id() {
        let cli = Cli::try_parse_from([
            "aicoder-tui",
            "--workspace",
            ".",
            "--session",
            "58ed33e6-26fc-4688-81ad-909d63af5ad7",
        ])
        .unwrap();

        assert_eq!(
            cli.session.as_deref(),
            Some("58ed33e6-26fc-4688-81ad-909d63af5ad7")
        );
    }
}
