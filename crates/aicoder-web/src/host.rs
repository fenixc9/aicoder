use aicoder_core::{
    Agent, AgentTurnOutcome, ModelProvider, RunControlConfig, RunHandle, SessionSelection,
    TurnExecutionContext,
    events::{EventBufferConfig, EventSubscription},
    session::{MemorySessionRepository, SessionRepository},
    tools::ApprovalSessions,
};
use aicoder_runtime::Runtime;
use anyhow::{Result, ensure};
use futures::FutureExt;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Notify, Semaphore},
    task::JoinHandle,
};

pub(crate) const MAX_ID: u64 = (1 << 53) - 1;
pub(crate) const MAX_BODY: usize = 1024 * 1024;
/// Private host authentication and a loopback origin. Neither is chosen by browser payloads.
pub struct WebOptions {
    pub(crate) token: String,
    pub(crate) address: SocketAddr,
    memory: bool,
    pub(crate) extensions: Option<Arc<aicoder_core::extensions::ExtensionSet>>,
}
impl WebOptions {
    /// The address must be the actual bound loopback address (not port zero).
    pub fn new(token: String, address: SocketAddr) -> Result<Self> {
        ensure!(
            (32..=256).contains(&token.len()) && token.bytes().all(|b| (33..=126).contains(&b)),
            "Web token must contain 32..256 printable ASCII bytes"
        );
        ensure!(
            address.ip().is_loopback() && address.port() != 0,
            "Web example requires a bound loopback address"
        );
        Ok(Self {
            token,
            address,
            memory: false,
            extensions: None,
        })
    }
    /// Immutable trusted native registrations; browser payloads cannot install code.
    pub fn extensions(mut self, extensions: Arc<aicoder_core::extensions::ExtensionSet>) -> Self {
        self.extensions = Some(extensions);
        self
    }
    /// Retain history in memory. Runtime session storage must also be disabled.
    pub fn memory_sessions(mut self) -> Self {
        self.memory = true;
        self
    }
}
#[derive(Clone)]
pub struct WebHost(pub(crate) Arc<Inner>);
pub(crate) struct Inner {
    pub options: WebOptions,
    pub(crate) runtime: Runtime,
    agent: Arc<Agent>,
    pub(crate) repository: Arc<dyn SessionRepository>,
    pub state: Mutex<State>,
    shutdown_gate: tokio::sync::Mutex<()>,
    pub streams: Arc<Semaphore>,
    approvals: ApprovalSessions,
}
pub(crate) struct State {
    pub closing: bool,
    adopted_session: Option<String>,
    lifecycle_shutdown_started: bool,
    pub last_id: u64,
    pub current: Option<Arc<Run>>,
    pub(crate) selection: SessionSelection,
    pub command: Option<Arc<crate::commands::CommandJob>>,
}
pub(crate) struct Run {
    pub handle: RunHandle,
    pub request_id: u64,
    pub ui: aicoder_core::ui::UiHost,
    pub events: Mutex<Option<EventSubscription>>,
    pub completion: Mutex<Option<Value>>,
    changed: Notify,
    owner: Mutex<Option<JoinHandle<()>>>,
}
impl Run {
    pub async fn done(&self) -> Value {
        loop {
            let changed = self.changed.notified();
            if let Some(result) = self.completion.lock().expect("Web completion").clone() {
                return result;
            }
            changed.await;
        }
    }
    pub fn snapshot(&self) -> Value {
        let s = self.handle.snapshot();
        json!({"version":1,"request_id":self.request_id,"run_id":s.run_id,"session_id":s.session_id,"status":s.status,"approvals":s.approvals,"result":*self.completion.lock().expect("Web completion"),"ui":self.ui.snapshot()})
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(run) = &self.state.lock().expect("Web state").current {
            run.ui.close();
            let _ = run.handle.cancel("Web host dropped");
        }
        if let Some(command) = &self.state.lock().expect("Web state").command {
            command.cancel("Web host dropped");
        }
        self.approvals.clear();
    }
}
impl WebHost {
    /// Construct the optional Web adapter using runtime's fixed host-selected provider route.
    pub fn new(runtime: Runtime, options: WebOptions) -> Result<Self> {
        let provider = Arc::new(runtime.session_client()?);
        Self::with_provider(runtime, options, provider)
    }
    /// Inject a provider for embedding, deterministic tests and benches; execution stays in core.
    pub fn with_provider(
        runtime: Runtime,
        options: WebOptions,
        provider: Arc<dyn ModelProvider>,
    ) -> Result<Self> {
        ensure!(
            !options.memory || runtime.config().session_dir.is_none(),
            "Memory Web sessions require RuntimeOverrides::session_dir = Setting::Clear"
        );
        let mut config = runtime.config().execution.core_config();
        let cap = Duration::from_secs(120);
        config.budget.max_duration = Some(config.budget.max_duration.map_or(cap, |d| d.min(cap)));
        config.budget.max_output_bytes = Some(
            config
                .budget
                .max_output_bytes
                .unwrap_or(8 * 1024 * 1024)
                .min(8 * 1024 * 1024),
        );
        config.budget.max_tool_calls = Some(config.budget.max_tool_calls.unwrap_or(32).min(32));
        let builder = if let Some(extensions) = &options.extensions {
            runtime.executor_builder_with_extensions(provider, extensions.clone())?
        } else {
            runtime.executor_builder(provider)?
        };
        let builder = builder.config(config);
        let executor = builder.build()?;
        let agent = Agent::new(executor, runtime.agent_config()).following_session_models();
        let agent = if !runtime.follows_session_reasoning()
            && runtime.config().options.reasoning_effort.is_none()
        {
            agent.recording_reasoning_defaults()
        } else {
            agent
        };
        let agent = Arc::new(if runtime.follows_session_reasoning() {
            agent.following_session_reasoning()
        } else {
            agent
        });
        let repository: Arc<dyn SessionRepository> = if options.memory {
            Arc::new(MemorySessionRepository::new())
        } else {
            Arc::new(runtime.sessions()?)
        };
        Ok(Self(Arc::new(Inner {
            options,
            shutdown_gate: tokio::sync::Mutex::new(()),
            runtime,
            agent,
            repository,
            state: Mutex::new(State {
                closing: false,
                adopted_session: None,
                lifecycle_shutdown_started: false,
                last_id: 0,
                current: None,
                selection: SessionSelection::New,
                command: None,
            }),
            approvals: ApprovalSessions::default(),
            streams: Arc::new(Semaphore::new(2)),
        })))
    }
    /// Create routes. The caller must await `shutdown` before tearing down its runtime.
    pub fn router(&self) -> axum::Router {
        crate::http::router(self.clone())
    }
    pub(crate) fn revoke(&self) -> bool {
        let state = self.0.state.lock().expect("Web state");
        let id = state
            .current
            .as_ref()
            .and_then(|run| run.handle.snapshot().session_id)
            .or_else(|| match &state.selection {
                SessionSelection::Existing(id) => Some(id.clone()),
                _ => None,
            });
        id.and_then(|id| {
            self.0
                .approvals
                .get(&self.0.runtime.config().workspace, &id)
        })
        .map(|session| session.revoke_all())
        .is_some()
    }
    /// Close admission, cancel the owned run and await authoritative execution/persistence.
    pub async fn shutdown(&self) {
        let _shutdown = self.0.shutdown_gate.lock().await;
        let (run, command) = {
            let mut state = self.0.state.lock().expect("Web state");
            state.closing = true;
            (state.current.clone(), state.command.clone())
        };
        if let Some(command) = &command {
            command.cancel("Web host shutdown");
        }
        if let Some(run) = run {
            run.ui.close();
            let _ = run.handle.cancel("Web host shutdown");
            let owner = run.owner.lock().expect("Web owner").take();
            if let Some(owner) = owner {
                let _ = owner.await;
            } else {
                run.done().await;
            }
        }
        if let Some(command) = command {
            command.join().await;
        }
        self.0.approvals.clear();
        let session = {
            let mut state = self.0.state.lock().expect("Web state");
            if state.lifecycle_shutdown_started {
                None
            } else {
                state.lifecycle_shutdown_started = true;
                state.adopted_session.clone()
            }
        };
        if self.0.agent.has_session_hooks()
            && let Some(id) = session
        {
            let report = self
                .0
                .agent
                .notify_session_shutdown(
                    self.0.repository.as_ref(),
                    &id,
                    aicoder_core::extensions::SessionShutdownReason::Quit,
                    aicoder_core::TurnExecutionContext::new(),
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
    }
    pub(crate) fn run(&self, id: &str) -> Option<Arc<Run>> {
        self.0
            .state
            .lock()
            .expect("Web state")
            .current
            .as_ref()
            .filter(|run| run.handle.run_id().to_string() == id)
            .cloned()
    }
    pub(crate) fn start(
        &self,
        id: u64,
        text: &str,
    ) -> std::result::Result<Arc<Run>, (&'static str, &'static str)> {
        let mut state = self.0.state.lock().expect("Web state");
        if state.closing {
            return Err(("closing", "Host is shutting down"));
        }
        if id == 0 || id > MAX_ID || id <= state.last_id {
            return Err((
                "stale_id",
                "Prompt IDs must strictly increase and fit a JavaScript-safe integer",
            ));
        }
        state.last_id = id;
        if state
            .command
            .as_ref()
            .is_some_and(|command| command.active())
            || state
                .current
                .as_ref()
                .is_some_and(|run| run.completion.lock().expect("Web completion").is_none())
        {
            return Err(("busy", "Wait for the current run to finish"));
        }
        if text.trim().is_empty() || text.len() > MAX_BODY {
            return Err(("invalid_input", "Prompt must be nonempty and fit 1 MiB"));
        }
        Ok(self.launch(
            &mut state,
            aicoder_core::AgentInput::from(text).with_source(aicoder_core::InputSource::Web),
        ))
    }
    pub(crate) fn launch(
        &self,
        state: &mut State,
        prompt: impl Into<aicoder_core::AgentPrompt>,
    ) -> Arc<Run> {
        let prompt = prompt.into();
        let ui = aicoder_core::ui::UiHost::interactive();
        let (handle, control) = self
            .0
            .agent
            .control_channel(
                TurnExecutionContext::new()
                    .with_approval_sessions(self.0.approvals.clone())
                    .with_ui(ui.client("extension").expect("fixed UI owner")),
                RunControlConfig {
                    handle_approvals: true,
                    ..Default::default()
                },
            )
            .expect("valid Web controls");
        let events = control
            .subscribe(EventBufferConfig::default())
            .expect("new Web subscription");
        let run = Arc::new(Run {
            handle,
            request_id: state.last_id,
            ui,
            events: Mutex::new(Some(events)),
            completion: Mutex::new(None),
            changed: Notify::new(),
            owner: Mutex::new(None),
        });
        state.current = Some(run.clone());
        let selection = state.selection.clone();
        let current = state.adopted_session.clone();
        let agent = self.0.agent.clone();
        let repository = self.0.repository.clone();
        let weak = Arc::downgrade(&self.0);
        let record = run.clone();
        let owner = tokio::spawn(async move {
            let outcome = std::panic::AssertUnwindSafe(agent.run_hosted_session_controlled(
                repository.as_ref(),
                current.as_deref(),
                selection,
                prompt,
                Arc::new(()),
                control,
            ))
            .catch_unwind()
            .await;
            let lifecycle = outcome
                .as_ref()
                .ok()
                .map(|r| r.lifecycle.clone())
                .unwrap_or_default();
            let outcome = outcome.map(|r| r.outcome);
            record.ui.close();
            let session = outcome
                .as_ref()
                .ok()
                .and_then(|outcome| outcome.session())
                .map(|s| s.id.clone())
                .or_else(|| record.handle.snapshot().session_id);
            let owner_failed = outcome.is_err();
            let mut result = match outcome {
                Ok(outcome) => completion(&record, &outcome),
                Err(_) => {
                    json!({"version":1,"run_id":record.handle.run_id(),"session_id":session,"status":"owner_failed","message":"Execution ended without an outcome; inspect recorded session"})
                }
            };
            if !lifecycle.is_empty() {
                result["session_lifecycle"] = json!(lifecycle);
            }
            if let Some(inner) = weak.upgrade() {
                let mut state = inner.state.lock().expect("Web state");
                if let Some(id) = session {
                    if owner_failed {
                        inner
                            .approvals
                            .remove(&inner.runtime.config().workspace, &id);
                    }
                    state.adopted_session = Some(id.clone());
                    state.selection = SessionSelection::Existing(id);
                }
                *record.completion.lock().expect("Web completion") = Some(result);
            } else {
                *record.completion.lock().expect("Web completion") = Some(result);
            }
            record.changed.notify_waiters();
        });
        *run.owner.lock().expect("Web owner") = Some(owner);
        run
    }
}
fn completion(run: &Run, outcome: &AgentTurnOutcome) -> Value {
    let progress = outcome.progress();
    let status = match outcome {
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
    let (stop, persistence_error) = match outcome {
        AgentTurnOutcome::Completed(_) => (None, None),
        AgentTurnOutcome::Aborted(v)
        | AgentTurnOutcome::Limited(v)
        | AgentTurnOutcome::Stopped(v)
        | AgentTurnOutcome::Failed(v) => (Some(&v.stop), v.persistence_error.as_ref()),
    };
    json!({"version":1,"run_id":run.handle.run_id(),"session_id":outcome.session().map(|s|&s.id),"status":status,"stop":stop,"persistence_error":persistence_error,"rounds":progress.rounds,"tool_calls":progress.tool_calls,"usage":progress.usage,"tokens":progress.tokens,"model_context":progress.model_context,"event_delivery":progress.event_delivery,"elapsed_ms":progress.elapsed.as_millis()})
}
