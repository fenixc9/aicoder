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
        })
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
    runtime: Runtime,
    agent: Arc<Agent>,
    repository: Arc<dyn SessionRepository>,
    pub state: Mutex<State>,
    pub streams: Arc<Semaphore>,
    approvals: ApprovalSessions,
}
pub(crate) struct State {
    pub closing: bool,
    pub last_id: u64,
    pub current: Option<Arc<Run>>,
    selection: SessionSelection,
}
pub(crate) struct Run {
    pub handle: RunHandle,
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
        json!({"version":1,"run_id":s.run_id,"session_id":s.session_id,"status":s.status,"approvals":s.approvals,"result":*self.completion.lock().expect("Web completion")})
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(run) = &self.state.lock().expect("Web state").current {
            let _ = run.handle.cancel("Web host dropped");
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
        let executor = runtime.executor_builder(provider)?.config(config).build()?;
        let agent =
            Arc::new(Agent::new(executor, runtime.agent_config()).following_session_models());
        let repository: Arc<dyn SessionRepository> = if options.memory {
            Arc::new(MemorySessionRepository::new())
        } else {
            Arc::new(runtime.sessions()?)
        };
        Ok(Self(Arc::new(Inner {
            options,
            runtime,
            agent,
            repository,
            state: Mutex::new(State {
                closing: false,
                last_id: 0,
                current: None,
                selection: SessionSelection::New,
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
        let run = {
            let mut state = self.0.state.lock().expect("Web state");
            state.closing = true;
            state.current.clone()
        };
        if let Some(run) = run {
            let _ = run.handle.cancel("Web host shutdown");
            let owner = run.owner.lock().expect("Web owner").take();
            if let Some(owner) = owner {
                let _ = owner.await;
            } else {
                run.done().await;
            }
        }
        self.0.approvals.clear();
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
            .current
            .as_ref()
            .is_some_and(|run| run.completion.lock().expect("Web completion").is_none())
        {
            return Err(("busy", "Wait for the current run to finish"));
        }
        if text.trim().is_empty() || text.len() > MAX_BODY {
            return Err(("invalid_input", "Prompt must be nonempty and fit 1 MiB"));
        }
        let prompt = self
            .0
            .runtime
            .prepare_prompt(text)
            .map_err(|_| {
                (
                    "invalid_input",
                    "Prompt expansion failed; inspect host resources",
                )
            })?
            .text;
        let (handle, control) = RunHandle::channel(
            TurnExecutionContext::new().with_approval_sessions(self.0.approvals.clone()),
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
            events: Mutex::new(Some(events)),
            completion: Mutex::new(None),
            changed: Notify::new(),
            owner: Mutex::new(None),
        });
        state.current = Some(run.clone());
        let selection = state.selection.clone();
        let agent = self.0.agent.clone();
        let repository = self.0.repository.clone();
        let weak = Arc::downgrade(&self.0);
        let record = run.clone();
        let owner = tokio::spawn(async move {
            let outcome = std::panic::AssertUnwindSafe(agent.run_with_session_controlled(
                repository.as_ref(),
                selection,
                prompt,
                Arc::new(()),
                control,
            ))
            .catch_unwind()
            .await;
            let session = outcome
                .as_ref()
                .ok()
                .and_then(|outcome| outcome.session())
                .map(|s| s.id.clone())
                .or_else(|| record.handle.snapshot().session_id);
            let owner_failed = outcome.is_err();
            let result = match outcome {
                Ok(outcome) => completion(&record, &outcome),
                Err(_) => {
                    json!({"version":1,"run_id":record.handle.run_id(),"session_id":session,"status":"owner_failed","message":"Execution ended without an outcome; inspect recorded session"})
                }
            };
            if let Some(inner) = weak.upgrade() {
                let mut state = inner.state.lock().expect("Web state");
                if let Some(id) = session {
                    if owner_failed {
                        inner
                            .approvals
                            .remove(&inner.runtime.config().workspace, &id);
                    }
                    state.selection = SessionSelection::Existing(id);
                }
                *record.completion.lock().expect("Web completion") = Some(result);
            } else {
                *record.completion.lock().expect("Web completion") = Some(result);
            }
            record.changed.notify_waiters();
        });
        *run.owner.lock().expect("Web owner") = Some(owner);
        Ok(run)
    }
}
fn completion(run: &Run, outcome: &AgentTurnOutcome) -> Value {
    let progress = outcome.progress();
    let status = match outcome {
        AgentTurnOutcome::Completed(_) => "completed",
        AgentTurnOutcome::Aborted(_) => "aborted",
        AgentTurnOutcome::Limited(_) => "limited",
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
