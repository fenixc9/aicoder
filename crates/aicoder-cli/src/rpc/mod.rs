//! Versioned local stdio adapter. All execution and authorization remain in core/runtime.
mod protocol;
mod transport;

use super::*;
use aicoder_core::{
    AgentRawEventEnvelope, AgentTurnOutcome, RunControlConfig, RunHandle, TurnExecutionContext,
    events::{EventBufferConfig, EventStreamError, EventSubscription},
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
    lifecycle: Arc<std::sync::Mutex<Vec<aicoder_core::session::SessionActionReport>>>,
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
    async fn cancel_and_wait(mut self) -> Option<AgentTurnOutcome> {
        let _ = self.handle.cancel("RPC transport closed");
        // The transport is unusable. Drop only observation, then await the execution owner.
        self.events = None;
        self.owner.await.ok()
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

type InputReply = (u64, Result<Value, Failure>);
type InputJobUpdate = Option<Result<InputReply, tokio::task::JoinError>>;
struct SessionJob {
    id: u64,
    context: TurnExecutionContext,
    owner: JoinHandle<Result<aicoder_core::session::SessionSelectionOutcome, String>>,
}
struct Host<'a> {
    runtime: &'a Runtime,
    agent: Arc<Agent>,
    repository: Arc<dyn SessionRepository>,
    selection: SessionSelection,
    adopted_session: Option<String>,
    approvals: ApprovalSessions,
    automatic: bool,
    active: Option<Active>,
    session_job: Option<SessionJob>,
    last: Option<Value>,
    closing: bool,
    input_jobs: tokio::task::JoinSet<InputReply>,
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
            adopted_session: None,
            approvals: ApprovalSessions::default(),
            automatic: cli.yes,
            active: None,
            session_job: None,
            last: None,
            closing: false,
            input_jobs: tokio::task::JoinSet::new(),
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
                "commands":["initialize","prompt","status","steer","follow_up","cancel","approve","select_session","cancel_session_selection","session_tree","session_search","permissions","revoke","shutdown"],
                "limits":{"input_line_bytes":input::MAX_INPUT,"frame_bytes":transport::MAX_FRAME,"output_queue_bytes":transport::MAX_BYTES,"output_queue_frames":transport::MAX_FRAMES,"output_stall_ms":transport::STALL.as_millis(),"max_request_id":protocol::MAX_ID,"active_runs":1},
                "model":self.runtime.config().provider.model,"automatic_approval":self.automatic}),
            ),
            Command::CancelSessionSelection { request_id } => {
                let job = self
                    .session_job
                    .as_ref()
                    .filter(|j| j.id == request_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!("No owned session selection matches request_id")
                    })?;
                job.context.cancel("RPC session selection cancelled");
                Ok(json!({"cancel_requested":true,"request_id":request_id}))
            }
            Command::Status => {
                let active=self.active.as_ref().map(|active|{
                    let snapshot=active.handle.snapshot();
                    json!({"run_id":snapshot.run_id,"session_id":snapshot.session_id,"revision":snapshot.revision,"status":snapshot.status,"pending_inputs":snapshot.pending_inputs,"approvals":snapshot.approvals})
                });
                Ok(
                    json!({"session_id":self.session_id(),"active":active,"last":self.last,"session_selection_request":self.session_job.as_ref().map(|j|j.id)}),
                )
            }
            Command::Prompt { text } => {
                if self.active.is_some() || self.session_job.is_some() {
                    return Err(CommandFailure(Failure::new(
                        "busy",
                        "Wait for run_finished before starting another turn",
                    ))
                    .into());
                }
                let prompt = self.prepare(&text)?;
                let context =
                    TurnExecutionContext::new().with_approval_sessions(self.approvals.clone());
                let (handle, control) = self.agent.control_channel(
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
                let current = self.adopted_session.clone();
                let lifecycle = Arc::new(std::sync::Mutex::new(Vec::new()));
                let captured = lifecycle.clone();
                let owner = tokio::spawn(async move {
                    let hosted = agent
                        .run_hosted_session_controlled(
                            repository.as_ref(),
                            current.as_deref(),
                            selection,
                            aicoder_core::AgentInput::from(prompt)
                                .with_source(aicoder_core::InputSource::Rpc),
                            Arc::new(()),
                            control,
                        )
                        .await;
                    *captured.lock().expect("RPC session observations") = hosted.lifecycle;
                    hosted.outcome
                });
                self.active = Some(Active {
                    handle,
                    events: Some(events),
                    owner,
                    lifecycle,
                });
                Ok(json!({"run_id":run_id}))
            }
            Command::Steer { .. } | Command::FollowUp { .. } => {
                anyhow::bail!("input commands require async dispatch")
            }
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
                anyhow::ensure!(
                    !self.agent.has_session_hooks(),
                    "Session selection requires asynchronous dispatch"
                );
                if self.active.is_some() || self.session_job.is_some() {
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
                if let Some(job) = &self.session_job {
                    job.context.cancel("RPC shutdown");
                }
                if let Some(active) = &self.active {
                    let _ = active.handle.cancel("RPC shutdown");
                }
                Ok(json!({"closing":true}))
            }
        }
    }
    fn begin_session_selection(
        &mut self,
        id: u64,
        session_id: Option<String>,
    ) -> Result<(), Failure> {
        if self.active.is_some() || self.session_job.is_some() {
            return Err(Failure::new(
                "busy",
                "Wait for the owned operation before selecting a session",
            ));
        }
        let current = match &self.selection {
            SessionSelection::Existing(id) => Some(id.clone()),
            _ => None,
        };
        let selection = session_id.map_or(SessionSelection::New, SessionSelection::Existing);
        let agent = self.agent.clone();
        let repository = self.repository.clone();
        let context = TurnExecutionContext::new().with_approval_sessions(self.approvals.clone());
        let owned = context.clone();
        let owner = tokio::spawn(async move {
            agent
                .select_session(repository.as_ref(), current.as_deref(), selection, owned)
                .await
                .map_err(|e| aicoder_core::redaction::sanitize_text(&e.to_string(), 2048))
        });
        self.session_job = Some(SessionJob { id, context, owner });
        Ok(())
    }
    fn finish_session_selection(
        &mut self,
        result: Result<aicoder_core::session::SessionSelectionOutcome, String>,
    ) -> Result<Value, Failure> {
        match result {
            Ok(output) => {
                self.approvals.clear();
                self.adopted_session = Some(output.session.metadata().id.clone());
                self.selection = SessionSelection::Existing(output.session.metadata().id.clone());
                self.last = None;
                Ok(json!({"session_id":self.session_id(),"session_lifecycle":output.reports}))
            }
            Err(message) => Err(Failure::new("session_selection_failed", message)),
        }
    }
    fn prepare(&self, text: &str) -> Result<String> {
        anyhow::ensure!(
            !text.trim().is_empty() && text.len() <= input::MAX_INPUT,
            "prompt must be nonempty and fit 1 MiB"
        );
        Ok(text.to_owned())
    }
    fn begin_input(
        &mut self,
        id: u64,
        run_id: &str,
        text: &str,
        follow: bool,
    ) -> Result<(), Failure> {
        let handle = self.control(run_id)?.clone();
        let text = self
            .prepare(text)
            .map_err(|_| Failure::new("invalid_input", "prompt must be nonempty and fit 1 MiB"))?;
        if self.input_jobs.len() >= 32 {
            return Err(Failure::new(
                "control_rejected",
                "too many pending input submissions",
            ));
        }
        let kind = if follow {
            aicoder_core::RunInputKind::FollowUp
        } else {
            aicoder_core::RunInputKind::Steer
        };
        let prepared = handle
            .begin_input(
                kind,
                aicoder_core::AgentInput::from(text).with_source(aicoder_core::InputSource::Rpc),
            )
            .map_err(|e| {
                Failure::new(
                    "control_rejected",
                    aicoder_core::redaction::sanitize_text(&e.to_string(), 2048),
                )
            })?;
        self.input_jobs.spawn(async move {
            let result = prepared
                .await
                .map(|result| match result {
                    aicoder_core::InputSubmission::Queued { input_id } => {
                        json!({"input_id":input_id})
                    }
                    aicoder_core::InputSubmission::Handled { by } => json!({"handled":by}),
                })
                .map_err(|error| {
                    Failure::new(
                        "control_rejected",
                        aicoder_core::redaction::sanitize_text(&error.to_string(), 2048),
                    )
                });
            (id, result)
        });
        Ok(())
    }
    fn finished(&mut self, outcome: AgentTurnOutcome) -> Value {
        let active = self.active.take().expect("owned run");
        if let Some(session) = outcome.session() {
            self.adopted_session = Some(session.id.clone());
            self.selection = SessionSelection::Existing(session.id.clone());
        }
        let progress = outcome.progress();
        let status = match &outcome {
            AgentTurnOutcome::Completed(_) => "completed",
            AgentTurnOutcome::Aborted(_) => "aborted",
            AgentTurnOutcome::Limited(_) => "limited",
            AgentTurnOutcome::Stopped(v)
                if matches!(v.stop, aicoder_core::RunStop::InputHandled(_)) =>
            {
                "handled"
            }
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
        let lifecycle = active.lifecycle.lock().expect("RPC session observations");
        if !lifecycle.is_empty() {
            result["session_lifecycle"] = json!(*lifecycle);
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
    if let Some(active) = host.active.take()
        && let Some(outcome) = active.cancel_and_wait().await
        && let Some(session) = outcome.session()
    {
        host.adopted_session = Some(session.id.clone());
        host.selection = SessionSelection::Existing(session.id.clone());
    }
    if let Some(job) = host.session_job.take() {
        job.context.cancel("RPC transport closed");
        if let Ok(result) = job.owner.await {
            let _ = host.finish_session_selection(result);
        }
    }
    host.input_jobs.abort_all();
    while host.input_jobs.join_next().await.is_some() {}
    host.approvals.clear();
    if host.agent.has_session_hooks()
        && let SessionSelection::Existing(id) = &host.selection
    {
        let report = host
            .agent
            .notify_session_shutdown(
                host.repository.as_ref(),
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
async fn serve(host: &mut Host<'_>) -> Result<()> {
    let mut lines = input::Lines::new()?;
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut output = transport::Output::new()?;
    let mut ticks = tokio::time::interval(std::time::Duration::from_millis(10));
    let mut last_id = 0;
    let mut interrupted = false;
    loop {
        output.flush_some()?;
        if host.closing
            && host.active.is_none()
            && host.session_job.is_none()
            && host.input_jobs.is_empty()
            && output.is_empty()
        {
            break;
        }
        enum Action {
            Tick,
            Input(Result<Option<String>>),
            Run(Update),
            InputPrepared(InputJobUpdate),
            SessionSelected(
                Result<
                    Result<aicoder_core::session::SessionSelectionOutcome, String>,
                    tokio::task::JoinError,
                >,
            ),
            Signal,
        }
        let action = tokio::select! {
            _=ticks.tick()=>Action::Tick,
            _=signal.recv()=>Action::Signal,
            line=lines.next_line(),if !host.closing=>Action::Input(line),
            result=host.input_jobs.join_next(),if !host.input_jobs.is_empty()=>Action::InputPrepared(result),
            selected=async{match &mut host.session_job{Some(job)=>(&mut job.owner).await,None=>std::future::pending().await}}=>Action::SessionSelected(selected),
            update=async {match &mut host.active {Some(active)=>active.next().await,None=>std::future::pending().await}}=>Action::Run(update),
        };
        match action {
            Action::Tick => {}
            Action::Signal => {
                interrupted = true;
                host.closing = true;
                if let Some(job) = &host.session_job {
                    job.context.cancel("RPC transport closing");
                }
                if let Some(active) = &host.active {
                    let _ = active.handle.cancel("Ctrl-C");
                }
            }
            Action::Input(Err(error)) => return Err(error),
            Action::Input(Ok(None)) => {
                host.closing = true;
                if let Some(job) = &host.session_job {
                    job.context.cancel("RPC transport closing");
                }
                if let Some(active) = &host.active {
                    let _ = active.handle.cancel("RPC stdin EOF");
                }
            }
            Action::Input(Ok(Some(line))) => {
                let response = match protocol::parse(&line, &mut last_id) {
                    Ok((id, Command::Steer { run_id, text })) => {
                        match host.begin_input(id, &run_id, &text, false) {
                            Ok(()) => continue,
                            Err(error) => Response::new(Some(id), Err(error)),
                        }
                    }
                    Ok((id, Command::FollowUp { run_id, text })) => {
                        match host.begin_input(id, &run_id, &text, true) {
                            Ok(()) => continue,
                            Err(error) => Response::new(Some(id), Err(error)),
                        }
                    }
                    Ok((id, Command::SelectSession { session_id }))
                        if host.agent.has_session_hooks() =>
                    {
                        match host.begin_session_selection(id, session_id) {
                            Ok(()) => continue,
                            Err(error) => Response::new(Some(id), Err(error)),
                        }
                    }
                    Ok((id, command)) => Response::new(Some(id), host.command(command)),
                    Err(response) => response,
                };
                output.push(&response)?;
            }
            Action::SessionSelected(result) => {
                let id = host.session_job.take().expect("session job").id;
                let result = match result {
                    Ok(result) => host.finish_session_selection(result),
                    Err(_) => Err(Failure::new(
                        "session_owner_failed",
                        "Session selection owner ended without a result; inspect stored sessions",
                    )),
                };
                output.push(&Response::new(Some(id), result))?;
            }
            Action::InputPrepared(Some(Ok((id, result)))) => {
                output.push(&Response::new(Some(id), result))?
            }
            Action::InputPrepared(Some(Err(_))) => {
                anyhow::bail!("input submission owner ended without a result")
            }
            Action::InputPrepared(None) => {}
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

#[cfg(test)]
mod input_tests {
    use super::*;
    use aicoder_core::{extensions::*, model::*};
    use aicoder_runtime::{
        ConfigDiscovery, RuntimeOverrides, Setting, Settings, resources::ResourceSettings,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Provider {
        started: tokio::sync::Notify,
        calls: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl ModelProvider for Provider {
        fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
            let mut spec = ModelSpec::new("fixture", model);
            spec.capabilities.tools = true;
            Ok(spec)
        }
        async fn generate(
            &self,
            _: ModelRequest,
            _: Arc<dyn ModelEventSink>,
            _: TurnExecutionContext,
        ) -> Result<ModelResponse, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            std::future::pending().await
        }
    }
    struct Raw {
        started: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl InputHook for Raw {
        async fn input(
            &self,
            input: InputHookInput<'_>,
            _: ExtensionCancellation,
        ) -> Result<InputAction> {
            assert_eq!(input.source, aicoder_core::InputSource::Rpc);
            if input.input.message().text_content() == "/consume" {
                return Ok(InputAction::Handled);
            }
            if input.delivery != InputDelivery::Initial {
                self.started.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(InputAction::Continue)
        }
    }
    fn runtime(root: &std::path::Path) -> Runtime {
        Runtime::load_with_environment(
            root,
            RuntimeOverrides {
                settings: Settings {
                    model: Setting::Value("offline".into()),
                    resources: ResourceSettings {
                        enabled: Setting::Value(false),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                session_dir: Setting::Clear,
                ..Default::default()
            },
            ConfigDiscovery {
                enabled: false,
                ..Default::default()
            },
            Arc::new(std::collections::BTreeMap::new()),
        )
        .unwrap()
    }
    fn setup(runtime: &Runtime, started: Arc<tokio::sync::Notify>, p: Arc<Provider>) -> Host<'_> {
        let cli = Cli::parse_from(["aicoder", "--no-session", "--yes"]);
        let mut host = Host::new(&cli, runtime).unwrap();
        let mut e = Extension::new("rpc-input", "1").unwrap();
        e.input_hook(HookSpec::new("raw"), Arc::new(Raw { started }))
            .unwrap();
        let mut r = ExtensionRegistry::new();
        r.install(e).unwrap();
        host.agent = Arc::new(Agent::new(
            runtime
                .executor_builder_with_extensions(p, Arc::new(r.finish()))
                .unwrap()
                .build()
                .unwrap(),
            runtime.agent_config(),
        ));
        host
    }
    #[tokio::test]
    async fn rpc_consumed_initial_input_reports_handled_without_model_io() {
        let root = tempfile::tempdir().unwrap();
        let runtime = runtime(root.path());
        let provider = Arc::new(Provider {
            started: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
        });
        let mut host = setup(
            &runtime,
            Arc::new(tokio::sync::Notify::new()),
            provider.clone(),
        );
        host.command(Command::Prompt {
            text: "/consume".into(),
        })
        .unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Update::Finished(Ok(outcome)) = host.active.as_mut().unwrap().next().await {
                    return outcome;
                }
            }
        })
        .await
        .unwrap();
        let report = host.finished(*outcome);
        assert_eq!(report["status"], "handled");
        assert_eq!(report["stop"]["kind"], "input_handled");
        assert_eq!(report["rounds"], 0);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn rpc_keeps_status_and_cancel_available_while_an_input_handler_waits() {
        let root = tempfile::tempdir().unwrap();
        let runtime = runtime(root.path());
        let started = Arc::new(tokio::sync::Notify::new());
        let provider = Arc::new(Provider {
            started: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
        });
        let mut host = setup(&runtime, started.clone(), provider.clone());
        let run = host
            .command(Command::Prompt {
                text: "initial".into(),
            })
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            provider.started.notified(),
        )
        .await
        .unwrap();
        let run_id = run["run_id"].as_str().unwrap().to_owned();
        host.begin_input(2, &run_id, "wait", false).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        assert_eq!(
            host.command(Command::Status).unwrap()["active"]["pending_inputs"],
            json!([])
        );
        host.command(Command::Cancel { run_id }).unwrap();
        let (_, response) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            host.input_jobs.join_next(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert!(response.is_err());
        host.active.take().unwrap().cancel_and_wait().await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
    struct SessionWait {
        started: Arc<tokio::sync::Notify>,
        ended: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl SessionHook for SessionWait {
        async fn before(
            &self,
            _: SessionHookInput<'_>,
            c: ExtensionCancellation,
        ) -> Result<SessionDecision> {
            let ended = self.ended.clone();
            tokio::spawn(async move {
                c.cancelled().await;
                ended.notify_one();
            });
            self.started.notify_one();
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn rpc_session_selection_keeps_status_shutdown_and_targeted_cancel_responsive() {
        let root = tempfile::tempdir().unwrap();
        let runtime = runtime(root.path());
        let provider = Arc::new(Provider {
            started: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
        });
        let cli = Cli::parse_from(["aicoder", "--no-session", "--yes"]);
        let mut host = Host::new(&cli, &runtime).unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let ended = Arc::new(tokio::sync::Notify::new());
        let mut e = Extension::new("rpc-session", "1").unwrap();
        e.session_hook(
            HookSpec::new("wait"),
            Arc::new(SessionWait {
                started: started.clone(),
                ended: ended.clone(),
            }),
        )
        .unwrap();
        let mut registry = ExtensionRegistry::new();
        registry.install(e).unwrap();
        host.agent = Arc::new(Agent::new(
            runtime
                .executor_builder_with_extensions(provider.clone(), Arc::new(registry.finish()))
                .unwrap()
                .build()
                .unwrap(),
            runtime.agent_config(),
        ));
        host.begin_session_selection(10, None).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .unwrap();
        assert_eq!(
            host.command(Command::Status).unwrap()["session_selection_request"],
            10
        );
        assert!(
            host.command(Command::Prompt {
                text: "must not run".into()
            })
            .is_err()
        );
        assert!(
            host.command(Command::CancelSessionSelection { request_id: 9 })
                .is_err()
        );
        host.command(Command::CancelSessionSelection { request_id: 10 })
            .unwrap();
        let job = host.session_job.take().unwrap();
        let result = job.owner.await.unwrap();
        assert!(host.finish_session_selection(result).is_err());
        assert!(host.session_id().is_none());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(host.repository.list(root.path()).unwrap().is_empty());
        tokio::time::timeout(std::time::Duration::from_secs(1), ended.notified())
            .await
            .unwrap();
        host.begin_session_selection(11, None).unwrap();
        started.notified().await;
        host.command(Command::Shutdown).unwrap();
        let job = host.session_job.take().unwrap();
        assert!(job.context.is_cancelled());
        assert!(job.owner.await.unwrap().is_err());
    }
}
