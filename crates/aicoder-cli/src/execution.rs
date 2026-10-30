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
    let initial = if !cli.repl && initial.is_none() {
        Some(cli.prompt.clone())
    } else {
        initial
    };
    if let Some(prompt) = &initial {
        anyhow::ensure!(
            prompt.len() <= input::MAX_INPUT && !prompt.trim().is_empty(),
            "prompt must be nonempty and fit 1 MiB"
        );
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
        if prompt.trim().is_empty() {
            continue;
        }
        let prepared = match runtime.prepare_prompt(&prompt) {
            Ok(prepared) => prepared,
            Err(error) if cli.repl => {
                eprintln!("{error}");
                continue;
            }
            Err(error) => return Err(error),
        };
        let handler: Arc<dyn AgentEventHandler> = if cli.json {
            json.as_ref().expect("JSON writer").clone()
        } else if cli.print {
            Arc::new(())
        } else {
            Arc::new(ConsoleEvents)
        };
        let context = TurnExecutionContext::new().with_approval_sessions(approvals.clone());
        let turn_selection = selection.clone();
        let work = async {
            match repository {
                Some(repo) => {
                    agent
                        .run_with_session_outcome(
                            repo,
                            turn_selection.clone(),
                            prepared.text,
                            handler,
                            context.clone(),
                        )
                        .await
                }
                None => {
                    agent
                        .run_outcome(prepared.text, handler, context.clone())
                        .await
                }
            }
        };
        tokio::pin!(work);
        let outcome = loop {
            tokio::select! {biased;_=signal.recv()=>context.cancel("Ctrl-C"),outcome=&mut work=>break outcome}
        };
        if let Some(session) = outcome.session() {
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
        let failed = !matches!(outcome, AgentTurnOutcome::Completed(_));
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
