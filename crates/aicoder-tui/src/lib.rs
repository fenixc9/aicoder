mod app;
mod commands;
mod editor;
mod extension_ui;
mod runtime;
mod theme;
mod ui;

use std::{
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(test)]
use aicoder_core::RunHandle;
use aicoder_core::{RunControlConfig, RunInputKind};
use aicoder_runtime::resources::ResourceSettings;
use aicoder_runtime::{
    ConfigDiscovery, ContextSettings, Runtime, RuntimeOverrides, Setting, Settings,
    environment_file,
};
use anyhow::{Context, Result};
use app::{App, AppEvent, Focus, TimelineItem};
use clap::Parser;
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste},
    event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use runtime::AgentRuntime;

#[derive(Debug, Parser)]
#[command(name = "aicoder-tui", about = "Interactive terminal UI for aicoder")]
struct Cli {
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    #[arg(long, conflicts_with_all = ["theme_file", "theme_package"])]
    theme: Option<String>,
    #[arg(long, conflicts_with = "theme_package")]
    theme_file: Option<PathBuf>,
    /// Use a declared installed theme asset: PACKAGE:PATH.
    #[arg(long)]
    theme_package: Option<String>,

    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    reasoning_effort: Option<aicoder_runtime::ReasoningOverride>,

    #[arg(long, value_name = "TOKENS")]
    context_window: Option<usize>,

    #[arg(long)]
    profile: Option<String>,
    #[arg(long, conflicts_with = "no_config")]
    config: Option<PathBuf>,
    #[arg(long, conflicts_with = "no_config")]
    user_config: Option<PathBuf>,
    #[arg(long)]
    no_config: bool,
    #[arg(long)]
    show_config: bool,

    #[arg(long)]
    env_file: Option<PathBuf>,
    #[arg(long)]
    no_resources: bool,
    /// Enable bounded JavaScript tool composition (requires the code-mode build feature).
    #[arg(long)]
    code_mode: bool,
    /// Offer only Code Mode to the model; leaf tools retain their host policies.
    #[arg(long, conflicts_with = "code_mode")]
    code_mode_only: bool,
    /// Explicit trusted stdio launch file (requires host-extensions for transports).
    #[arg(long)]
    extension_config: Option<PathBuf>,
    /// Activate installed, selected package executables after exact host trust validation.
    #[arg(long)]
    activate_packages: bool,
    #[arg(long, requires = "activate_packages")]
    package_programs: Option<PathBuf>,
    #[arg(long)]
    no_parent_context: bool,
    #[arg(long)]
    no_skills: bool,
    #[arg(long)]
    no_templates: bool,
    /// Open an existing session by its full ID.
    #[arg(long, value_name = "ID")]
    session: Option<String>,
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("Failed to enable terminal raw mode")?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            return Err(error).context("Failed to enter alternate terminal screen");
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
    }
}

pub async fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    let application = Runtime::load_with_environment(
        &cli.workspace,
        RuntimeOverrides {
            settings: Settings {
                provider_profile: Setting::from_option(cli.profile.clone()),
                model: Setting::from_option(cli.model.clone()),
                options: aicoder_runtime::ModelSettings {
                    reasoning_effort: cli
                        .reasoning_effort
                        .map(|v| v.setting())
                        .unwrap_or(Setting::Inherit),
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
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: !cli.no_config,
            user_file: cli.user_config.clone(),
            project_file: cli.config.clone(),
        },
        environment_file(cli.env_file.as_deref())?,
    )?;
    if cli.show_config {
        println!("{}", serde_json::to_string_pretty(application.config())?);
        return Ok(());
    }
    let palette = if let Some(selection) = &cli.theme_package {
        theme::Theme::from_package(&application, selection)?
    } else if let Some(path) = &cli.theme_file {
        theme::Theme::load(path)?
    } else {
        theme::Theme::named(cli.theme.as_deref().unwrap_or("dark"))?
    };
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
        let work =
            application.activate_host_extensions(&manifest, programs.as_ref(), context.clone());
        tokio::pin!(work);
        let result = loop {
            tokio::select! {biased;
                _=tokio::signal::ctrl_c()=>context.cancel("TUI startup cancelled"),
                result=&mut work=>break result,
            }
        };
        Some(result?)
    } else {
        None
    };
    let application = if let Some(active) = &activation {
        application.with_extensions(active.extensions())?
    } else {
        application
    };
    let outcome = async {
        init_logging(application.state_root()?)?;
        let model = application.config().provider.model.clone();
        let initial_reasoning = application.config().options.reasoning_effort;
        let runtime = AgentRuntime::new(application)?;
        let mut app = App::new(runtime.sessions()?);
        app.reasoning_effort = initial_reasoning;
        app.theme = palette;
        if let Some(session_id) = cli.session {
            if cli.model.is_some() || cli.profile.is_some() {
                runtime
                    .select_model(&session_id, &model, runtime.execution_context())
                    .await?;
            }
            if let Some(choice) = cli.reasoning_effort {
                runtime
                    .select_reasoning(&session_id, choice.0, runtime.execution_context())
                    .await?;
            }
            let session = runtime.open_run_session(&session_id)?;
            app.selected_session = app
                .sessions
                .iter()
                .position(|candidate| candidate.id == session_id)
                .context("Selected session is missing from the workspace session list")?;
            app.load_run_session(&session);
        }

        let _guard = TerminalGuard::enter()?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend).context("Failed to initialize terminal")?;
        terminal.clear()?;
        run(&mut terminal, app, runtime, &model).await
    }
    .await;
    match activation {
        Some(active) => active.finish(outcome).await,
        None => outcome,
    }
}

/// Run the terminal adapter with host-owned provider and immutable native extensions.
/// The caller retains executable activation owners and awaits their cleanup after this returns.
pub async fn run_with_provider(
    application: Runtime,
    provider: Arc<dyn aicoder_core::ModelProvider>,
    extensions: Arc<aicoder_core::extensions::ExtensionSet>,
) -> Result<()> {
    let model = application.config().provider.model.clone();
    let reasoning = application.config().options.reasoning_effort;
    let runtime = AgentRuntime::with_provider(application, provider, Some(extensions))?;
    let mut app = App::new(runtime.sessions()?);
    app.reasoning_effort = reasoning;
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    run(&mut terminal, app, runtime, &model).await
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut app: App,
    mut runtime: AgentRuntime,
    model: &str,
) -> Result<()> {
    let result = drive(terminal, &mut app, &mut runtime, model).await;
    app.cancel("TUI exited");
    while let Some(run) = &mut app.run {
        let event = run.recv().await;
        app.apply(event);
    }
    while let Some(reference) = &mut app.reference {
        let event = reference.recv().await;
        app.apply(event);
    }
    app.input_jobs.abort_all();
    while app.input_jobs.join_next().await.is_some() {}
    result
}
async fn drive(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    runtime: &mut AgentRuntime,
    model: &str,
) -> Result<()> {
    let mut terminal_events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        app.composer_width = usize::from(terminal.size()?.width.saturating_sub(2).max(1));
        app.refresh_ui();
        app.bounded_preview();
        if let Some(handle) = &app.active_handle
            && let Some(info) = handle.model_context()
        {
            app.model_context = Some(info);
        }
        terminal.draw(|frame| ui::draw(frame, app, app.model_label.as_deref().unwrap_or(model)))?;
        if app.should_quit {
            break;
        }
        enum Action {
            Tick,
            Agent(AppEvent),
            Reference(AppEvent),
            InputPrepared(Option<Result<crate::app::InputJobResult, tokio::task::JoinError>>),
            Terminal(Option<io::Result<Event>>),
        }
        let action = tokio::select! {
            _=tick.tick()=>Action::Tick,
            event=async {match &mut app.run {Some(run)=>run.recv().await,None=>std::future::pending().await}}=>Action::Agent(event),
            event=async{match &mut app.reference{Some(job)=>job.recv().await,None=>std::future::pending().await}}=>Action::Reference(event),
            result=app.input_jobs.join_next(),if !app.input_jobs.is_empty()=>Action::InputPrepared(result),
            event=terminal_events.next()=>Action::Terminal(event),
        };
        match action {
            Action::Tick => {}
            Action::InputPrepared(Some(Ok(result))) => app.finish_input_submission(result),
            Action::InputPrepared(Some(Err(error))) => app.timeline.push(TimelineItem::Error(
                format!("Input submission ended: {error}"),
            )),
            Action::InputPrepared(None) => {}
            Action::Reference(event) => app.apply(event),
            Action::Agent(event) => {
                let submit = if let AppEvent::CommandFinished(report) = &event {
                    if app
                        .active_context
                        .as_ref()
                        .is_some_and(|context| context.is_cancelled())
                    {
                        None
                    } else if let Ok(aicoder_core::extensions::CommandOutput::Submit(input)) =
                        report.as_ref()
                    {
                        Some(input.clone())
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let AppEvent::Agent(envelope) = &event
                    && let aicoder_core::AgentRawEvent::AgentStarted { model } = &envelope.event
                {
                    app.model_spec = runtime.model_spec(model).ok();
                    app.host_window = runtime.context_window();
                }

                let finished = matches!(
                    event,
                    AppEvent::TurnFinished(_)
                        | AppEvent::CompactionFinished(_)
                        | AppEvent::ModelSelected(_)
                        | AppEvent::CommandFinished(_)
                        | AppEvent::RunDropped
                );
                if finished {
                    app.refresh_ui();
                }
                app.apply(event);
                if let Some(input) = submit
                    && let Err(error) = start_input(app, runtime, input)
                {
                    app.timeline.push(TimelineItem::Error(error.to_string()));
                }
                if finished {
                    app.sessions = runtime.sessions().unwrap_or_else(|error| {
                        tracing::error!(error=%format!("{error:#}"),"Failed to refresh sessions");
                        Vec::new()
                    });
                }
            }
            Action::Terminal(event) => match event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                    handle_key(app, runtime, key)?
                }
                Some(Ok(Event::Paste(text))) => handle_paste(app, &text),
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error).context("Failed to read terminal event"),
                None => break,
            },
        }
    }
    Ok(())
}

fn start_input(
    app: &mut App,
    runtime: &AgentRuntime,
    prompt: aicoder_core::AgentInput,
) -> Result<()> {
    let prompt = if prompt.source() == aicoder_core::InputSource::Sdk {
        prompt.with_source(aicoder_core::InputSource::Interactive)
    } else {
        prompt
    };
    let context = app.ui_context(runtime.execution_context());
    let (handle, control) = runtime.control_channel(
        context.clone(),
        RunControlConfig {
            handle_approvals: true,
            max_input_bytes: 2 * 1024 * 1024,
            max_pending_bytes: 4 * 1024 * 1024,
            ..Default::default()
        },
    )?;
    let run = runtime.start_turn(
        app.session_selection.clone(),
        prompt.clone(),
        app.pending_reasoning,
        control,
    )?;
    app.pending_reasoning = None;
    app.begin_turn(prompt.message().text_content(), context);
    app.active_handle = Some(handle);
    app.run = Some(run);
    Ok(())
}

fn handle_paste(app: &mut App, text: &str) {
    if app.approval.is_none()
        && let Some(ui) = &mut app.extension_ui
        && ui.asking()
    {
        ui.paste(text);
        return;
    }
    if app.approval.is_some() || app.confirm_delete || app.focus != Focus::Input {
        return;
    }
    if !app.input.insert_text(text) {
        app.timeline.push(TimelineItem::Error(
            "Paste exceeds the 64 KiB draft budget; draft unchanged.".into(),
        ));
    } else {
        app.history.edited();
        app.slash_input_changed();
    }
}

fn handle_key(app: &mut App, runtime: &mut AgentRuntime, key: KeyEvent) -> Result<()> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if app.is_running() || app.reference.is_some() {
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
            KeyCode::Char('s') | KeyCode::Char('S')
                if app
                    .approval
                    .as_ref()
                    .is_some_and(|approval| approval.session.is_some()) =>
            {
                app.resolve_approval_decision(aicoder_core::tools::ApprovalDecision::AllowSession)
            }
            KeyCode::PageDown => {
                if let Some(approval) = &mut app.approval {
                    approval.scroll = approval.scroll.saturating_add(8);
                }
            }
            KeyCode::PageUp => {
                if let Some(approval) = &mut app.approval {
                    approval.scroll = approval.scroll.saturating_sub(8);
                }
            }
            KeyCode::Tab => {
                if let Some(approval) = &mut app.approval {
                    approval.show_arguments = !approval.show_arguments;
                    approval.scroll = 0;
                }
            }
            _ => {}
        }
        return Ok(());
    }
    if let Some(ui) = &mut app.extension_ui
        && ui.asking()
    {
        if let Err(error) = ui.key(key, app.composer_width.saturating_mul(4) / 5) {
            app.timeline.push(TimelineItem::Error(error.to_string()));
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
    if key.code == KeyCode::Esc && (app.is_running() || app.reference.is_some()) {
        app.cancel("Escape pressed");
        return Ok(());
    }
    match key.code {
        KeyCode::Tab if app.focus == Focus::Input && app.selected_slash_command().is_some() => {
            app.complete_selected_slash_command();
        }
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
        _ if app.focus == Focus::Input => handle_input_key(app, runtime, key),
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
                app.load_run_session(&runtime.open_run_session(&session.id)?);
            }
        }
        KeyCode::Char('n') => app.select_new_session(),
        KeyCode::Char('d') if !app.sessions.is_empty() => app.confirm_delete = true,
        _ => {}
    }
    Ok(())
}

fn handle_input_key(app: &mut App, runtime: &mut AgentRuntime, key: KeyEvent) {
    match key.code {
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.input.insert('\n');
            app.history.edited();
            app.slash_input_changed();
        }
        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.input.insert('\n');
            app.history.edited();
            app.slash_input_changed();
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(text) = app.history.previous(app.input.value()) {
                app.input.replace(text);
                app.slash_input_changed();
            }
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(text) = app.history.next() {
                app.input.replace(text);
                app.slash_input_changed();
            }
        }
        KeyCode::Enter => {
            if app.selected_slash_command().is_some() {
                app.complete_selected_slash_command();
            }
            let prompt = app.input.take();
            app.slash_input_changed();
            if prompt.trim().is_empty() && app.references.is_empty() && app.images.is_empty() {
                return;
            }
            match commands::parse(&prompt) {
                Some(Ok(commands::SlashCommand::Extension)) => {
                    if app.is_running() {
                        app.input.replace(prompt);
                        app.timeline.push(TimelineItem::Notice(
                            "Wait for the current operation before running a command.".into(),
                        ));
                        return;
                    }
                    let args = commands::arguments(&prompt);
                    if args.is_empty() {
                        app.timeline.push(TimelineItem::Notice(
                            serde_json::to_string_pretty(&runtime.commands())
                                .expect("serializable command specs"),
                        ));
                        return;
                    }
                    let (name, raw) = args.split_once(char::is_whitespace).unwrap_or((args, "{}"));
                    let arguments = match serde_json::from_str(raw) {
                        Ok(value) => value,
                        Err(_) => {
                            app.timeline.push(TimelineItem::Error(
                                "Expected command arguments as JSON.".into(),
                            ));
                            app.input.replace(prompt);
                            return;
                        }
                    };
                    let context = app.ui_context(runtime.execution_context());
                    match runtime.start_command(
                        app.session_selection.clone(),
                        name.into(),
                        arguments,
                        context.clone(),
                    ) {
                        Ok(run) => {
                            app.begin_command(context);
                            app.run = Some(run);
                        }
                        Err(error) => {
                            app.close_ui();
                            app.timeline.push(TimelineItem::Error(error.to_string()));
                        }
                    }
                    return;
                }
                Some(Ok(commands::SlashCommand::Image)) => {
                    if app.reference.is_some() {
                        app.input.replace(prompt);
                        app.timeline.push(TimelineItem::Notice(
                            "Wait for the pending capture or cancel it.".into(),
                        ));
                        return;
                    }
                    app.reference = Some(runtime.start_image(
                        commands::arguments(&prompt).into(),
                        runtime.execution_context(),
                    ));
                    return;
                }
                Some(Ok(commands::SlashCommand::Unimage)) => {
                    let args = commands::arguments(&prompt);
                    if args.is_empty() || args == "all" {
                        app.reference = None;
                        app.images.clear();
                    } else {
                        match args.parse::<usize>() {
                            Ok(i) if i > 0 && i <= app.images.len() => {
                                app.images.remove(i - 1);
                            }
                            _ => app.timeline.push(TimelineItem::Error(
                                "Use /unimage INDEX (1-based) or /unimage all".into(),
                            )),
                        }
                    }
                    return;
                }

                Some(Ok(commands::SlashCommand::File)) => {
                    if app.reference.is_some() {
                        app.input.replace(prompt);
                        app.timeline.push(TimelineItem::Notice(
                            "Wait for the pending file page or cancel it.".into(),
                        ));
                        return;
                    }
                    app.reference = Some(runtime.start_reference(
                        commands::arguments(&prompt).into(),
                        runtime.execution_context(),
                    ));
                    return;
                }
                Some(Ok(commands::SlashCommand::Detach)) => {
                    let args = commands::arguments(&prompt);
                    if args.is_empty() || args == "all" {
                        app.reference = None;
                        app.references.clear();
                    } else {
                        match args.parse::<usize>() {
                            Ok(i) if i > 0 && i <= app.references.len() => {
                                app.references.remove(i - 1);
                            }
                            _ => app.timeline.push(TimelineItem::Error(
                                "Use /detach INDEX (1-based) or /detach all".into(),
                            )),
                        }
                    }
                    return;
                }
                Some(Ok(commands::SlashCommand::Theme)) => {
                    match runtime.theme(commands::arguments(&prompt)) {
                        Ok(theme) => app.theme = theme,
                        Err(error) => app.timeline.push(TimelineItem::Error(error.to_string())),
                    }
                    return;
                }
                Some(Ok(commands::SlashCommand::Usage)) => {
                    app.timeline.push(TimelineItem::Notice(serde_json::to_string_pretty(&serde_json::json!({"usage":app.usage,"model_context":app.model_context,"model_window":app.model_spec.as_ref().and_then(|s|s.context_tokens),"host_window":app.host_window,"base_cost_estimate":app.cost_estimate(),"estimate_is_billed":false})).unwrap_or_default()));
                    return;
                }

                Some(Ok(commands::SlashCommand::Thinking)) => {
                    if app.is_running() {
                        app.input.replace(prompt);
                        app.timeline.push(TimelineItem::Notice(
                            "Finish or cancel the operation before changing reasoning.".into(),
                        ));
                        return;
                    }
                    let args = commands::arguments(&prompt);
                    if args.is_empty() {
                        match runtime
                            .reasoning_summary(&app.session_selection, app.pending_reasoning)
                        {
                            Ok(text) => app.timeline.push(TimelineItem::Notice(text)),
                            Err(error) => app.timeline.push(TimelineItem::Error(error.to_string())),
                        };
                        return;
                    }
                    let choice = match args.parse::<aicoder_runtime::ReasoningOverride>() {
                        Ok(v) => v,
                        Err(error) => {
                            app.input.replace(prompt);
                            app.timeline.push(TimelineItem::Error(error));
                            return;
                        }
                    };
                    let context = app.ui_context(runtime.execution_context());
                    app.run = Some(runtime.start_reasoning_selection(
                        app.session_selection.clone(),
                        choice.0,
                        context.clone(),
                    ));
                    app.begin_reasoning_selection(context);
                    return;
                }

                Some(Ok(commands::SlashCommand::Exit)) => {
                    handle_slash_command(app, &prompt);
                    return;
                }
                Some(Ok(commands::SlashCommand::Resources)) => {
                    match runtime.resource_summary() {
                        Ok(summary) => app.timeline.push(TimelineItem::Notice(summary)),
                        Err(error) => app.timeline.push(TimelineItem::Error(error.to_string())),
                    }
                    return;
                }
                Some(Ok(
                    command @ (commands::SlashCommand::Tree
                    | commands::SlashCommand::Search
                    | commands::SlashCommand::SearchNext
                    | commands::SlashCommand::Rename
                    | commands::SlashCommand::Label
                    | commands::SlashCommand::Unlabel
                    | commands::SlashCommand::Checkout
                    | commands::SlashCommand::Branch
                    | commands::SlashCommand::Recover
                    | commands::SlashCommand::Model),
                )) => {
                    if app.is_running() {
                        app.input.replace(prompt);
                        app.timeline.push(TimelineItem::Notice(
                            "Finish or cancel the current operation before navigating.".into(),
                        ));
                        return;
                    }
                    let args = commands::arguments(&prompt);
                    if command == commands::SlashCommand::Model {
                        if let aicoder_core::SessionSelection::Existing(id) =
                            app.session_selection.clone()
                        {
                            let context = app.ui_context(runtime.execution_context());
                            app.run = Some(runtime.start_model_selection(
                                id.clone(),
                                args.into(),
                                context.clone(),
                            ));
                            app.begin_model_selection(context);
                        } else {
                            app.timeline
                                .push(TimelineItem::Error("Select a stored session first".into()));
                        }
                    } else {
                        match runtime.navigate(&app.session_selection, command, args) {
                            Ok((session, notice)) => {
                                if let Some(session) = session {
                                    app.load_run_session(&session);
                                    app.sessions = runtime.sessions().unwrap_or_default();
                                }
                                app.timeline.push(TimelineItem::Notice(notice));
                            }
                            Err(error) => app.timeline.push(TimelineItem::Error(error.to_string())),
                        }
                    }
                    return;
                }
                Some(Ok(commands::SlashCommand::Compact)) => {
                    if app.is_running() {
                        app.input.replace(prompt);
                        app.timeline.push(TimelineItem::Notice(
                            "Finish or cancel the current operation before compacting.".into(),
                        ));
                    } else if let aicoder_core::SessionSelection::Existing(id) =
                        app.session_selection.clone()
                    {
                        let context = app.ui_context(runtime.execution_context());
                        match runtime.start_compaction(id.clone(), context.clone()) {
                            Ok(run) => {
                                app.begin_compaction(context);
                                app.run = Some(run);
                            }
                            Err(error) => app.timeline.push(TimelineItem::Error(error.to_string())),
                        }
                    } else {
                        app.timeline.push(TimelineItem::Notice(
                            "Select a stored session before compacting.".into(),
                        ));
                    }
                    return;
                }
                Some(Ok(commands::SlashCommand::Permissions | commands::SlashCommand::Revoke)) => {
                    if app.is_running() {
                        app.input.replace(prompt);
                        app.timeline.push(TimelineItem::Notice("Wait for the current run, or cancel it before changing session permissions.".into()));
                    } else if matches!(
                        commands::parse(&prompt),
                        Some(Ok(commands::SlashCommand::Revoke))
                    ) {
                        runtime.revoke_permissions(&app.session_selection);
                        app.timeline.push(TimelineItem::Notice(
                            "Session grants cleared. New operations require approval.".into(),
                        ));
                    } else {
                        let summary = runtime.permissions(&app.session_selection).map_or_else(
                            || "No session grants.".into(),
                            |snapshot| {
                                if snapshot.grants.is_empty() {
                                    return "No session grants.".into();
                                }
                                let grants = snapshot
                                    .grants
                                    .iter()
                                    .map(|grant| {
                                        format!(
                                            "{} ({:?}, all arguments)",
                                            grant.tool, grant.capability
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                format!(
                                    "Session {:?} / {:?}\n{}",
                                    snapshot.session.session_id, snapshot.session.workspace, grants
                                )
                            },
                        );
                        app.timeline.push(TimelineItem::Notice(summary));
                    }
                    return;
                }
                Some(Ok(commands::SlashCommand::Reload)) => {
                    if app.is_running() {
                        app.input.replace(prompt);
                        app.timeline.push(TimelineItem::Notice(
                            "Wait for the current run before reloading resources.".into(),
                        ));
                    } else {
                        match runtime.reload_resources() {
                            Ok(()) => app.timeline.push(TimelineItem::Notice(
                                "Resources reloaded for the next run.".into(),
                            )),
                            Err(error) => app.timeline.push(TimelineItem::Error(error.to_string())),
                        }
                    }
                    return;
                }
                Some(Err(error)) if commands::registered(&prompt) => {
                    app.timeline.push(TimelineItem::Error(error.to_string()));
                    app.input.replace(prompt);
                    return;
                }
                _ => {}
            }
            if app.reference.is_some() {
                app.input.replace(prompt);
                app.timeline.push(TimelineItem::Notice(
                    "File capture is pending; submit after it completes or cancel it.".into(),
                ));
                return;
            }
            let raw_prompt = prompt.clone();
            if !app.images.is_empty() && !runtime.has_input_hooks() {
                match runtime.supports_images(&app.session_selection) {
                    Ok(true) => {}
                    Ok(false) => {
                        app.input.replace(raw_prompt);
                        app.timeline.push(TimelineItem::Error("Selected model does not declare image input; choose a vision model before submitting.".into()));
                        return;
                    }
                    Err(error) => {
                        app.input.replace(raw_prompt);
                        app.timeline.push(TimelineItem::Error(error.to_string()));
                        return;
                    }
                }
            }
            let prompt = match app.compose_input(&prompt) {
                Ok(input) => input,
                Err(error) => {
                    app.input.replace(raw_prompt);
                    app.timeline.push(TimelineItem::Error(error.to_string()));
                    return;
                }
            };
            if app.is_running() {
                let kind = if key.modifiers.contains(KeyModifiers::ALT) {
                    RunInputKind::FollowUp
                } else {
                    RunInputKind::Steer
                };
                if app.queue_input(kind, prompt) {
                    app.history.record(&raw_prompt);
                    app.references.clear();
                    app.images.clear();
                } else {
                    app.input.replace(raw_prompt);
                }
                return;
            }
            match start_input(app, runtime, prompt) {
                Ok(()) => {
                    app.history.record(&raw_prompt);
                    app.references.clear();
                    app.images.clear();
                }
                Err(error) => {
                    app.input.replace(raw_prompt);
                    app.timeline.push(TimelineItem::Error(error.to_string()));
                }
            }
        }
        KeyCode::Char(character)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.input.insert(character);
            app.history.edited();
            app.slash_input_changed();
        }
        KeyCode::Backspace => {
            app.input.backspace();
            app.history.edited();
            app.slash_input_changed();
        }
        KeyCode::Delete => {
            app.input.delete();
            app.history.edited();
            app.slash_input_changed();
        }
        KeyCode::Up if !app.slash_suggestions().is_empty() => app.select_previous_slash_command(),
        KeyCode::Down if !app.slash_suggestions().is_empty() => app.select_next_slash_command(),
        KeyCode::Esc if !app.slash_suggestions().is_empty() => app.dismiss_slash_menu(),
        KeyCode::Up => {
            if !app.input.vertical(app.composer_width, false)
                && let Some(text) = app.history.previous(app.input.value())
            {
                app.input.replace(text);
            }
            app.slash_input_changed();
        }
        KeyCode::Down => {
            if !app.input.vertical(app.composer_width, true)
                && let Some(text) = app.history.next()
            {
                app.input.replace(text);
            }
            app.slash_input_changed();
        }
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => app.input.document_home(),
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => app.input.document_end(),
        KeyCode::Left => app.input.move_left(),
        KeyCode::Right => app.input.move_right(),
        KeyCode::Home => app.input.move_home(),
        KeyCode::End => app.input.move_end(),
        _ => {}
    }
}

fn handle_slash_command(app: &mut App, input: &str) -> bool {
    let Some(command) = commands::parse(input) else {
        return false;
    };
    match command {
        Ok(commands::SlashCommand::Exit) => app.should_quit = true,
        Ok(
            commands::SlashCommand::Extension
            | commands::SlashCommand::Resources
            | commands::SlashCommand::Reload
            | commands::SlashCommand::Permissions
            | commands::SlashCommand::Revoke
            | commands::SlashCommand::Compact
            | commands::SlashCommand::Tree
            | commands::SlashCommand::Search
            | commands::SlashCommand::SearchNext
            | commands::SlashCommand::Rename
            | commands::SlashCommand::Label
            | commands::SlashCommand::Unlabel
            | commands::SlashCommand::Checkout
            | commands::SlashCommand::Branch
            | commands::SlashCommand::Recover
            | commands::SlashCommand::Model
            | commands::SlashCommand::Thinking
            | commands::SlashCommand::Image
            | commands::SlashCommand::Unimage
            | commands::SlashCommand::File
            | commands::SlashCommand::Detach
            | commands::SlashCommand::Theme
            | commands::SlashCommand::Usage,
        ) => {}
        Err(error) => {
            app.timeline.push(TimelineItem::Error(error.to_string()));
            app.scroll_back = 0;
        }
    }
    true
}

fn init_logging(root: &Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
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
use aicoder_core::TurnExecutionContext;

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn rejected_template_steering_preserves_the_original_invocation() {
        use std::{collections::BTreeMap, sync::Arc};
        let workspace = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join(".aicoder/prompts")).unwrap();
        std::fs::write(
            workspace.path().join(".aicoder/prompts/review.md"),
            "Review $1",
        )
        .unwrap();
        let application = Runtime::load_with_environment(
            workspace.path(),
            Default::default(),
            ConfigDiscovery::default(),
            Arc::new(BTreeMap::from([
                (
                    "AICODER_HOME".into(),
                    global.path().to_string_lossy().to_string(),
                ),
                ("AICODER_PROXY".into(), "disabled".into()),
            ])),
        )
        .unwrap();
        let mut runtime = AgentRuntime::new(application).unwrap();
        let (handle, _control) = RunHandle::channel(
            TurnExecutionContext::new(),
            RunControlConfig {
                max_pending_inputs: 1,
                ..Default::default()
            },
        )
        .unwrap();
        handle
            .steer(aicoder_core::model::Message::text(
                aicoder_core::model::MessageRole::User,
                "already queued",
            ))
            .unwrap();
        let mut app = App::new(vec![]);
        app.begin_turn("active".into(), TurnExecutionContext::new());
        app.active_handle = Some(handle);
        app.input.replace("/review file.rs".into());
        handle_input_key(
            &mut app,
            &mut runtime,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert_eq!(app.input.value(), "/review file.rs");
    }

    #[test]
    fn reload_command_is_refused_during_a_run_without_erasing_the_draft() {
        use std::{collections::BTreeMap, sync::Arc};
        let workspace = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();
        let application = Runtime::load_with_environment(
            workspace.path(),
            Default::default(),
            ConfigDiscovery::default(),
            Arc::new(BTreeMap::from([
                (
                    "AICODER_HOME".into(),
                    global.path().to_string_lossy().to_string(),
                ),
                ("AICODER_PROXY".into(), "disabled".into()),
            ])),
        )
        .unwrap();
        let mut runtime = AgentRuntime::new(application).unwrap();
        let mut app = App::new(vec![]);
        app.begin_turn("active".into(), TurnExecutionContext::new());
        app.input.replace("/reload".into());
        handle_input_key(
            &mut app,
            &mut runtime,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert_eq!(app.input.value(), "/reload");
        assert!(
            app.timeline.iter().any(
                |item| matches!(item,TimelineItem::Notice(text) if text.contains("current run"))
            )
        );
    }

    #[test]
    fn exit_command_quits_without_creating_a_timeline_message() {
        let mut app = App::new(Vec::new());

        assert!(handle_slash_command(&mut app, "/exit"));

        assert!(app.should_quit);
        assert!(app.timeline.is_empty());
    }

    #[test]
    fn unknown_slash_command_is_not_sent_to_the_agent() {
        let mut app = App::new(Vec::new());

        assert!(handle_slash_command(&mut app, "/missing"));

        assert!(!app.should_quit);
        assert!(matches!(
            app.timeline.as_slice(),
            [TimelineItem::Error(message)] if message == "Unknown command: /missing"
        ));
    }

    #[test]
    fn tab_completion_replaces_the_query_and_closes_the_menu() {
        let mut app = App::new(Vec::new());
        for character in "/ex".chars() {
            app.input.insert(character);
        }

        assert!(app.complete_selected_slash_command());

        assert_eq!(app.input.value(), "/exit");
        assert!(app.slash_suggestions().is_empty());
        assert_eq!(
            commands::parse(app.input.value()),
            Some(Ok(commands::SlashCommand::Exit))
        );
    }
    #[test]
    fn bracketed_paste_is_draft_data_and_cannot_answer_modal_questions() {
        let mut app = App::new(vec![]);
        handle_paste(&mut app, "/exit\ny\n");
        assert_eq!(app.input.value(), "/exit\ny\n");
        assert!(!app.should_quit);
        app.confirm_delete = true;
        handle_paste(&mut app, "y");
        assert!(app.confirm_delete);
        assert_eq!(app.input.value(), "/exit\ny\n");
        app.confirm_delete = false;
        app.approval = Some(app::PendingApproval {
            id: aicoder_core::ApprovalRequestId {
                run_id: "fixture".into(),
                sequence: 1,
            },
            invocation: aicoder_core::tools::ToolInvocation {
                call_id: "call".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({}),
                capability: aicoder_core::tools::ToolCapability::Mutating,
                preview: None,
            },
            scroll: 0,
            show_arguments: false,
            session: None,
        });
        handle_paste(&mut app, "y\n");
        assert!(app.approval.is_some());
        assert_eq!(app.input.value(), "/exit\ny\n");
    }
}
