//! Versioned local stdio adapter. All execution and authorization remain in core/runtime.
mod protocol;
mod transport;

use super::*;
use aicoder_core::{
    AgentRawEventEnvelope, AgentTurnOutcome, RunControlConfig, RunHandle, TurnExecutionContext,
    events::{EventBufferConfig, EventStreamError, EventSubscription},
    model::{Message, MessageRole},
    session::{MemorySessionRepository, SessionNavigator},
};
use protocol::{Command, Failure, Response, VERSION};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::task::JoinHandle;

struct Active {
    handle: RunHandle,
    events: Option<EventSubscription>,
    owner: JoinHandle<AgentTurnOutcome>,
}
enum Update {
    Event(Arc<AgentRawEventEnvelope>),
    Observation(EventStreamError),
    Finished(Result<Box<AgentTurnOutcome>, tokio::task::JoinError>),
}
impl Active {
    async fn next(&mut self) -> Update {
        if let Some(events) = &mut self.events {
            match events.recv().await {
                Ok(Some(event)) => return Update::Event(event),
                Err(error) => {
                    self.events = None;
                    return Update::Observation(error);
                }
                Ok(None) => self.events = None,
            }
        }
        Update::Finished((&mut self.owner).await.map(Box::new))
    }
    async fn cancel_and_wait(mut self) {
        let _ = self.handle.cancel("RPC transport closed");
        // The transport is unusable. Drop only observation, then await the execution owner.
        self.events = None;
        let _ = self.owner.await;
    }
}
#[derive(Serialize)]
struct Notification<'a, T: Serialize> {
    version: u32,
    r#type: &'static str,
    #[serde(flatten)]
    body: &'a T,
}
#[derive(Serialize)]
struct EventBody<'a> {
    event: &'a AgentRawEventEnvelope,
}
#[derive(Serialize)]
struct ObservationBody {
    run_id: String,
    error: EventStreamError,
}
#[derive(Serialize)]
struct FinishedBody<'a> {
    result: &'a Value,
}

struct Host<'a> {
    runtime: &'a Runtime,
    agent: Arc<Agent>,
    repository: Arc<dyn SessionRepository>,
    selection: SessionSelection,
    approvals: ApprovalSessions,
    automatic: bool,
    active: Option<Active>,
    last: Option<Value>,
    closing: bool,
}
impl<'a> Host<'a> {
    fn new(cli: &Cli, runtime: &'a Runtime) -> Result<Self> {
        let builder = runtime.executor_builder(Arc::new(runtime.session_client()?))?;
        let executor = if cli.yes {
            builder.approval(AllowAllApproval).build()?
        } else {
            builder.build()?
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
        let repository: Arc<dyn SessionRepository> = if cli.no_session {
            Arc::new(MemorySessionRepository::new())
        } else {
            Arc::new(runtime.sessions()?)
        };
        let selection = if cli.continue_session {
            SessionSelection::ContinueMostRecent
        } else if let Some(id) = &cli.session {
            SessionSelection::Existing(id.clone())
        } else {
            SessionSelection::New
        };
        if let SessionSelection::Existing(id) = &selection {
            SessionNavigator::new(repository.as_ref(), &runtime.config().workspace)?.info(id)?;
        }
        Ok(Self {
            runtime,
            agent: Arc::new(agent),
            repository,
            selection,
            approvals: ApprovalSessions::default(),
            automatic: cli.yes,
            active: None,
            last: None,
            closing: false,
        })
    }
    fn session_id(&self) -> Option<String> {
        self.active
            .as_ref()
            .and_then(|active| active.handle.snapshot().session_id)
            .or_else(|| match &self.selection {
                SessionSelection::Existing(id) => Some(id.clone()),
                _ => None,
            })
    }
    fn control(&self, id: &str) -> Result<&RunHandle, Failure> {
        self.active
            .as_ref()
            .filter(|active| active.handle.run_id().to_string() == id)
            .map(|active| &active.handle)
            .ok_or_else(|| Failure::new("unknown_run", "No owned active run matches run_id"))
    }
    fn command(&mut self, command: Command) -> Result<Value, Failure> {
        self.execute(command).map_err(|error| {
            if let Some(failure) = error.downcast_ref::<CommandFailure>() {
                return Failure::new(failure.0.code, failure.0.message.clone());
            }
            Failure::new(
                "command_failed",
                aicoder_core::redaction::sanitize_text(&error.to_string(), 2048),
            )
        })
    }
    fn execute(&mut self, command: Command) -> Result<Value> {
        match command {
            Command::Initialize => Ok(
                json!({"protocol_version":VERSION,"event_schema_version":aicoder_core::events::EVENT_SCHEMA_VERSION,
                "commands":["initialize","prompt","status","steer","follow_up","cancel","approve","select_session","session_tree","session_search","permissions","revoke","shutdown"],
                "limits":{"input_line_bytes":input::MAX_INPUT,"frame_bytes":transport::MAX_FRAME,"output_queue_bytes":transport::MAX_BYTES,"output_queue_frames":transport::MAX_FRAMES,"output_stall_ms":transport::STALL.as_millis(),"max_request_id":protocol::MAX_ID,"active_runs":1},
                "model":self.runtime.config().provider.model,"automatic_approval":self.automatic}),
            ),
            Command::Status => {
                let active=self.active.as_ref().map(|active|{
                    let snapshot=active.handle.snapshot();
                    json!({"run_id":snapshot.run_id,"session_id":snapshot.session_id,"revision":snapshot.revision,"status":snapshot.status,"pending_inputs":snapshot.pending_inputs,"approvals":snapshot.approvals})
                });
                Ok(json!({"session_id":self.session_id(),"active":active,"last":self.last}))
            }
            Command::Prompt { text } => {
                if self.active.is_some() {
                    return Err(CommandFailure(Failure::new(
                        "busy",
                        "Wait for run_finished before starting another turn",
                    ))
                    .into());
                }
                let prompt = self.prepare(&text)?;
                let context =
                    TurnExecutionContext::new().with_approval_sessions(self.approvals.clone());
                let (handle, control) = RunHandle::channel(
                    context,
                    RunControlConfig {
                        handle_approvals: !self.automatic,
                        ..Default::default()
                    },
                )?;
                let events = control.subscribe(EventBufferConfig::default())?;
                let run_id = handle.run_id();
                let agent = self.agent.clone();
                let repository = self.repository.clone();
                let selection = self.selection.clone();
                let owner = tokio::spawn(async move {
                    agent
                        .run_with_session_controlled(
                            repository.as_ref(),
                            selection,
                            prompt,
                            Arc::new(()),
                            control,
                        )
                        .await
                });
                self.active = Some(Active {
                    handle,
                    events: Some(events),
                    owner,
                });
                Ok(json!({"run_id":run_id}))
            }
            Command::Steer { run_id, text } => self.queue_input(&run_id, &text, false),
            Command::FollowUp { run_id, text } => self.queue_input(&run_id, &text, true),
            Command::Cancel { run_id } => {
                self.control(&run_id)
                    .map_err(CommandFailure)?
                    .cancel("RPC cancel")?;
                Ok(json!({"cancellation_requested":true}))
            }
            Command::Approve {
                run_id,
                approval_id,
                decision,
            } => {
                self.control(&run_id)
                    .map_err(CommandFailure)?
                    .respond_approval_decision(&approval_id, decision)?;
                Ok(json!({"answered":true}))
            }
            Command::SelectSession { session_id } => {
                if self.active.is_some() {
                    return Err(CommandFailure(Failure::new(
                        "busy",
                        "Cannot select a session while a run is owned",
                    ))
                    .into());
                }
                if let Some(id) = &session_id {
                    SessionNavigator::new(
                        self.repository.as_ref(),
                        &self.runtime.config().workspace,
                    )?
                    .info(id)?;
                }
                self.approvals.clear();
                self.selection = match session_id {
                    Some(id) => SessionSelection::Existing(id),
                    None => SessionSelection::New,
                };
                self.last = None;
                Ok(json!({"session_id":self.session_id()}))
            }
            Command::SessionTree {
                session_id,
                limit,
                cursor,
            } => {
                let page = SessionNavigator::new(
                    self.repository.as_ref(),
                    &self.runtime.config().workspace,
                )?
                .tree(&session_id, limit, cursor.as_ref())?;
                Ok(serde_json::to_value(page)?)
            }
            Command::SessionSearch {
                session_id,
                query,
                limit,
                cursor,
            } => {
                let page = SessionNavigator::new(
                    self.repository.as_ref(),
                    &self.runtime.config().workspace,
                )?
                .search(&session_id, &query, limit, cursor.as_ref())?;
                Ok(serde_json::to_value(page)?)
            }
            Command::Permissions => {
                let snapshot = self
                    .session_id()
                    .and_then(|id| self.approvals.get(&self.runtime.config().workspace, &id))
                    .map(|session| session.snapshot());
                Ok(json!({"permissions":snapshot}))
            }
            Command::Revoke => {
                let revoked = self
                    .session_id()
                    .and_then(|id| self.approvals.get(&self.runtime.config().workspace, &id))
                    .map(|session| session.revoke_all())
                    .is_some();
                Ok(json!({"revoked":revoked}))
            }
            Command::Shutdown => {
                self.closing = true;
                if let Some(active) = &self.active {
                    let _ = active.handle.cancel("RPC shutdown");
                }
                Ok(json!({"closing":true}))
            }
        }
    }
    fn prepare(&self, text: &str) -> Result<String> {
        anyhow::ensure!(
            !text.trim().is_empty() && text.len() <= input::MAX_INPUT,
            "prompt must be nonempty and fit 1 MiB"
        );
        Ok(self.runtime.prepare_prompt(text)?.text)
    }
    fn queue_input(&self, run_id: &str, text: &str, follow: bool) -> Result<Value> {
        let handle = self.control(run_id).map_err(CommandFailure)?;
        let prompt = self.prepare(text)?;
        let message = Message::text(MessageRole::User, prompt);
        let id = if follow {
            handle.follow_up(message)
        } else {
            handle.steer(message)
        }
        .map_err(|error| CommandFailure(Failure::new("control_rejected", error.to_string())))?;
        Ok(json!({"input_id":id}))
    }
    fn finished(&mut self, outcome: AgentTurnOutcome) -> Value {
        let active = self.active.take().expect("owned run");
        if let Some(session) = outcome.session() {
            self.selection = SessionSelection::Existing(session.id.clone());
        }
        let progress = outcome.progress();
        let status = match &outcome {
            AgentTurnOutcome::Completed(_) => "completed",
            AgentTurnOutcome::Aborted(_) => "aborted",
            AgentTurnOutcome::Limited(_) => "limited",
            AgentTurnOutcome::Stopped(_) => "stopped",
            AgentTurnOutcome::Failed(_) => "failed",
        };
        let (stop, persistence_error) = match &outcome {
            AgentTurnOutcome::Completed(_) => (None, None),
            AgentTurnOutcome::Aborted(v)
            | AgentTurnOutcome::Limited(v)
            | AgentTurnOutcome::Stopped(v)
            | AgentTurnOutcome::Failed(v) => (Some(&v.stop), v.persistence_error.as_ref()),
        };
        let mut result = json!({"run_id":active.handle.run_id(),"session_id":outcome.session().map(|s|&s.id),"status":status,"stop":stop,"persistence_error":persistence_error,
            "rounds":progress.rounds,"tool_calls":progress.tool_calls,"usage":progress.usage,"tokens":progress.tokens,"output_bytes":progress.output_bytes,"elapsed_ms":progress.elapsed.as_millis(),"model_context":progress.model_context,"event_delivery":progress.event_delivery,
            "pending_inputs":progress.pending_inputs.iter().map(|v|json!({"run_id":v.run_id,"id":v.id,"kind":v.kind})).collect::<Vec<_>>()});
        if !progress.auxiliary.is_empty() {
            result["auxiliary"] = json!(progress.auxiliary);
            result["primary_usage"] = json!(progress.primary_usage);
        }
        self.last = Some(result.clone());
        result
    }
}
#[derive(Debug)]
struct CommandFailure(Failure);
impl std::fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.message)
    }
}
impl std::error::Error for CommandFailure {}

pub async fn run(cli: &Cli, runtime: &Runtime) -> Result<()> {
    let mut host = Host::new(cli, runtime)?;
    let result = serve(&mut host).await;
    if let Some(active) = host.active.take() {
        active.cancel_and_wait().await;
    }
    host.approvals.clear();
    result
}
async fn serve(host: &mut Host<'_>) -> Result<()> {
    let mut lines = input::Lines::new()?;
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut output = transport::Output::new()?;
    let mut ticks = tokio::time::interval(std::time::Duration::from_millis(10));
    let mut last_id = 0;
    let mut interrupted = false;
    loop {
        output.flush_some()?;
        if host.closing && host.active.is_none() && output.is_empty() {
            break;
        }
        enum Action {
            Tick,
            Input(Result<Option<String>>),
            Run(Update),
            Signal,
        }
        let action = tokio::select! {
            _=ticks.tick()=>Action::Tick,
            _=signal.recv()=>Action::Signal,
            line=lines.next_line(),if !host.closing=>Action::Input(line),
            update=async {match &mut host.active {Some(active)=>active.next().await,None=>std::future::pending().await}}=>Action::Run(update),
        };
        match action {
            Action::Tick => {}
            Action::Signal => {
                interrupted = true;
                host.closing = true;
                if let Some(active) = &host.active {
                    let _ = active.handle.cancel("Ctrl-C");
                }
            }
            Action::Input(Err(error)) => return Err(error),
            Action::Input(Ok(None)) => {
                host.closing = true;
                if let Some(active) = &host.active {
                    let _ = active.handle.cancel("RPC stdin EOF");
                }
            }
            Action::Input(Ok(Some(line))) => {
                let response = match protocol::parse(&line, &mut last_id) {
                    Ok((id, command)) => Response::new(Some(id), host.command(command)),
                    Err(response) => response,
                };
                output.push(&response)?;
            }
            Action::Run(Update::Event(event)) => output.push(&Notification {
                version: VERSION,
                r#type: "event",
                body: &EventBody { event: &event },
            })?,
            Action::Run(Update::Observation(error)) => output.push(&Notification {
                version: VERSION,
                r#type: "observation_error",
                body: &ObservationBody {
                    run_id: host
                        .active
                        .as_ref()
                        .expect("active")
                        .handle
                        .run_id()
                        .to_string(),
                    error,
                },
            })?,
            Action::Run(Update::Finished(Ok(outcome))) => {
                let result = host.finished(*outcome);
                output.push(&Notification {
                    version: VERSION,
                    r#type: "run_finished",
                    body: &FinishedBody { result: &result },
                })?;
            }
            Action::Run(Update::Finished(Err(_))) => {
                host.active = None;
                anyhow::bail!(
                    "RPC execution owner ended without an outcome; inspect persisted session"
                )
            }
        }
    }
    if interrupted {
        return Err(input::Interrupted.into());
    }
    Ok(())
}
