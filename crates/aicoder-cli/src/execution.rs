//! CLI transport/presentation over the single core Agent execution path.
use super::*;
use aicoder_core::{AgentTurnOutcome, TurnExecutionContext, session::MemorySessionRepository};
use std::{io::IsTerminal, sync::Mutex};
struct JsonEvents {
    output: Mutex<std::fs::File>,
    error: Mutex<Option<String>>,
}
impl JsonEvents {
    fn new() -> Result<Self> {
        Ok(Self {
            output: Mutex::new(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/stdout")?,
            ),
            error: Mutex::new(None),
        })
    }
}
impl AgentEventHandler for JsonEvents {
    fn on_raw_event(&self, event: &aicoder_core::AgentRawEventEnvelope) {
        let failed = self.error.lock().expect("JSON output").is_some();
        if failed {
            return;
        }
        let mut out = self.output.lock().expect("JSON output writer");
        let result = serde_json::to_writer(&mut *out, event)
            .map_err(io::Error::other)
            .and_then(|()| out.write_all(b"\n"))
            .and_then(|()| out.flush());
        if let Err(failure) = result {
            *self.error.lock().expect("JSON output") = Some(failure.to_string());
        }
    }
}
pub async fn run(cli: &Cli, explicit: bool, runtime: &Runtime) -> Result<()> {
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let payload_stdin =
        cli.stdin || !cli.repl && !explicit && cli.input.is_none() && !io::stdin().is_terminal();
    let initial = if payload_stdin {
        Some(input::Lines::new()?.all(&mut signal).await?)
    } else if explicit {
        Some(cli.prompt.clone())
    } else {
        cli.input.clone()
    };
    let initial = if initial.is_none() && !cli.images.is_empty() {
        Some(String::new())
    } else {
        initial
    };
    let initial = if !cli.repl && initial.is_none() {
        Some(cli.prompt.clone())
    } else {
        initial
    };
    if let Some(prompt) = &initial {
        anyhow::ensure!(
            prompt.len() <= input::MAX_INPUT
                && (!prompt.trim().is_empty() || !cli.images.is_empty()),
            "prompt must be nonempty and fit 1 MiB"
        );
    }
    anyhow::ensure!(cli.images.len() <= 4, "at most four initial images");
    let mut pending_images = Vec::new();
    for path in &cli.images {
        let path = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("image path requires UTF-8"))?;
        let context = TurnExecutionContext::new();
        let limits = aicoder_runtime::images::ImageLimits {
            max_output_bytes: (1024 * 1024 / cli.images.len()).min(512 * 1024),
            ..Default::default()
        };
        let result = cancellable(
            context.clone(),
            runtime.read_image(path, limits, context.clone()),
        )
        .await?;
        if context.is_cancelled() {
            return Err(input::Interrupted.into());
        }
        let image = result?;
        eprintln!(
            "[Image] {}",
            aicoder_core::redaction::sanitize_text(&image.label(), 2048)
        );
        pending_images.push(image);
    }
    let builder = runtime.executor_builder(Arc::new(runtime.session_client()?))?;
    let executor = if cli.yes {
        builder.approval(AllowAllApproval).build()?
    } else {
        builder
            .approval(ConsoleApproval {
                terminal: payload_stdin || cli.repl || cli.json || cli.print,
            })
            .build()?
    };
    let agent = Agent::new(executor, runtime.agent_config());
    let agent = if !runtime.follows_session_reasoning()
        && runtime.config().options.reasoning_effort.is_none()
    {
        agent.recording_reasoning_defaults()
    } else {
        agent
    };
    let agent = if runtime.follows_session_reasoning() {
        agent.following_session_reasoning()
    } else {
        agent
    };
    let agent = if cli.model.is_none() && cli.profile.is_none() {
        agent.following_session_models()
    } else {
        agent
    };
    let disk = if cli.no_session {
        None
    } else {
        Some(runtime.sessions()?)
    };
    let memory = MemorySessionRepository::new();
    let repository: Option<&dyn SessionRepository> = disk
        .as_ref()
        .map(|repo| repo as &dyn SessionRepository)
        .or_else(|| cli.repl.then_some(&memory));
    let mut selection = if cli.continue_session {
        SessionSelection::ContinueMostRecent
    } else if let Some(id) = &cli.session {
        SessionSelection::Existing(id.clone())
    } else {
        SessionSelection::New
    };
    let mut adopted_session: Option<String> = None;
    let approvals = ApprovalSessions::default();
    let json = if cli.json {
        Some(Arc::new(JsonEvents::new()?))
    } else {
        None
    };
    let mut lines = if cli.repl {
        Some(input::Lines::new()?)
    } else {
        None
    };
    let mut next = initial;
    let result:Result<()> = async {
    loop {
        let prompt = if let Some(prompt) = next.take() {
            prompt
        } else if let Some(lines) = &mut lines {
            if io::stdin().is_terminal() {
                eprint!("aicoder> ");
                io::stderr().flush()?;
            }
            match lines.line(&mut signal).await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    eprintln!("{error}");
                    break;
                }
            }
        } else {
            break;
        };
        if cli.repl && matches!(prompt.trim(), "/exit" | "/quit") {
            break;
        }
        if prompt.trim().is_empty() && pending_images.is_empty() {
            continue;
        }
        let prepared_input = match aicoder_runtime::images::input(prompt, &pending_images) {
            Ok(input) => input.with_source(aicoder_core::InputSource::Interactive),
            Err(error) if cli.repl => {
                eprintln!("{error}");
                continue;
            }
            Err(error) => return Err(error),
        };
        pending_images.clear();
        let handler: Arc<dyn AgentEventHandler> = if cli.json {
            json.as_ref().expect("JSON writer").clone()
        } else if cli.print {
            Arc::new(())
        } else {
            Arc::new(ConsoleEvents)
        };
        let context = TurnExecutionContext::new().with_approval_sessions(approvals.clone());
        let turn_selection = selection.clone();
        let turn_current=adopted_session.clone();
        let work = async {
            match repository {
                Some(repo) => {
                    agent
                        .run_hosted_session_outcome(
                            repo,
                            turn_current.as_deref(),
                            turn_selection.clone(),
                            prepared_input,
                            handler,
                            context.clone(),
                        )
                        .await
                }
                None => aicoder_core::HostedSessionOutcome {
                    outcome: agent
                        .run_outcome(prepared_input, handler, context.clone())
                        .await,
                    lifecycle: Vec::new(),
                },
            }
        };
        tokio::pin!(work);
        let outcome = loop {
            tokio::select! {biased;_=signal.recv()=>context.cancel("Ctrl-C"),outcome=&mut work=>break outcome}
        };
        let lifecycle_failures = outcome
            .lifecycle
            .iter()
            .map(|r| r.delivery.failures.len())
            .sum::<usize>();
        if lifecycle_failures > 0 {
            eprintln!(
                "Session adopted; {lifecycle_failures} lifecycle observations were incomplete."
            );
        }
        let outcome = outcome.outcome;
        if let Some(session) = outcome.session() {
            adopted_session = Some(session.id.clone());
            selection = SessionSelection::Existing(session.id.clone());
            if disk.is_some() {
                eprintln!("Session: {}", session.id);
            }
        }
        if let Some(error) = json
            .as_ref()
            .and_then(|json| json.error.lock().expect("JSON output").take())
        {
            anyhow::bail!("JSON output failed: {error}");
        }
        if outcome.progress().event_delivery.has_failures() {
            anyhow::bail!(
                "Live event delivery incomplete: {}",
                serde_json::to_string(&outcome.progress().event_delivery)?
            );
        }
        if cli.print {
            let text = outcome
                .progress()
                .messages
                .iter()
                .skip(outcome.progress().input_messages)
                .rev()
                .find(|message| {
                    message.role == aicoder_core::model::MessageRole::Assistant
                        && message.tool_calls().next().is_none()
                })
                .map(|message| message.text_content())
                .unwrap_or_default();
            if !text.is_empty() {
                writeln!(io::stdout().lock(), "{text}")?;
            }
        }
        if !cli.json && !cli.print {
            if let Some(limit) = cli.token_budget {
                println!(
                    "Token budget: {}/{limit}{}",
                    outcome.progress().tokens.used,
                    if outcome.progress().tokens.estimated {
                        " (estimated)"
                    } else {
                        ""
                    }
                );
            }
            println!(
                "Tokens: input={}, output={}, total={}",
                number(outcome.progress().usage.input_tokens),
                number(outcome.progress().usage.output_tokens),
                number(outcome.progress().usage.total_tokens)
            );
        }
        if let Some(info) = &outcome.progress().model_context
            && let Some(count) = &info.last_input_count
        {
            eprintln!(
                "[Context] input={} {:?}, {}",
                count.tokens, count.accuracy, count.method
            );
            if let Some(reason) = count.fallback {
                eprintln!("[Context] fallback={reason:?}");
            }
        }
        if !cli.repl && context.is_cancelled() && context.reason().as_ref() == "Ctrl-C" {
            return Err(input::Interrupted.into());
        }
        let handled = matches!(&outcome,AgentTurnOutcome::Stopped(turn) if matches!(turn.stop,aicoder_core::RunStop::InputHandled(_)));
        if handled && !cli.json && !cli.print {
            eprintln!("Input handled by extension");
        }
        let failed = !matches!(outcome, AgentTurnOutcome::Completed(_)) && !handled;
        if failed {
            if cli.repl {
                eprintln!("{}", outcome.into_result().unwrap_err());
            } else {
                return outcome.into_result().map(|_| ());
            }
        }
        if !cli.repl {
            break;
        }
    }
    Ok(())
    }.await;
    if agent.has_session_hooks()
        && let (Some(repository), Some(id)) = (repository, adopted_session.as_deref())
    {
        let report = agent
            .notify_session_shutdown(
                repository,
                id,
                aicoder_core::extensions::SessionShutdownReason::Quit,
                TurnExecutionContext::new(),
            )
            .await;
        if report.as_ref().is_err()
            || report
                .as_ref()
                .is_ok_and(|r| !r.delivery.failures.is_empty())
        {
            eprintln!("Session shutdown observations were incomplete.");
        }
    }
    result
}
fn number(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".into(), |value| value.to_string())
}

pub async fn cancellable<T>(
    context: TurnExecutionContext,
    work: impl std::future::Future<Output = T>,
) -> Result<T> {
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::pin!(work);
    loop {
        tokio::select! {biased;_=signal.recv()=>context.cancel("Ctrl-C"),result=&mut work=>return Ok(result)}
    }
}
