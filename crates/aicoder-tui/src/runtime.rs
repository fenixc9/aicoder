use aicoder_runtime::session::JsonlSessionRepository;
use std::{path::PathBuf, sync::Arc};

use aicoder_core::{
    Agent, RunControl, SessionSelection, TurnExecutionContext,
    session::{SessionInfo, SessionRepository},
    tools::{ApprovalSessions, ApprovalSnapshot},
};
use aicoder_core::{
    AgentTurnOutcome,
    events::{EventBufferConfig, EventSubscription},
};
use anyhow::Result;
use tokio::sync::oneshot;

use crate::app::AppEvent;

pub struct ActiveRun {
    events: Option<EventSubscription>,
    completion: RunCompletion,
}
enum RunCompletion {
    Command(oneshot::Receiver<Result<aicoder_core::extensions::CommandOutput, String>>),
    Turn(oneshot::Receiver<AgentTurnOutcome>),
    HostedTurn(oneshot::Receiver<aicoder_core::HostedSessionOutcome>),
    Compaction(oneshot::Receiver<aicoder_core::summary::ManualCompactionReport>),

    Model(oneshot::Receiver<Result<aicoder_core::session::ModelSelectionReport, String>>),
    Navigation(oneshot::Receiver<Result<SessionChange, String>>),
}
impl ActiveRun {
    pub async fn recv(&mut self) -> AppEvent {
        if let Some(events) = &mut self.events {
            match events.recv().await {
                Ok(Some(event)) => return AppEvent::Agent((*event).clone()),
                Err(error) => {
                    self.events = None;
                    return AppEvent::ObservationFailed(error.to_string());
                }
                Ok(None) => self.events = None,
            }
        }
        match &mut self.completion {
            RunCompletion::Command(receiver) => match receiver.await {
                Ok(report) => AppEvent::CommandFinished(Box::new(report)),
                Err(_) => AppEvent::RunDropped,
            },
            RunCompletion::Navigation(receiver) => match receiver.await {
                Ok(result) => AppEvent::SessionChanged(Box::new(result)),
                Err(_) => AppEvent::RunDropped,
            },
            RunCompletion::Model(receiver) => match receiver.await {
                Ok(report) => AppEvent::ModelSelected(Box::new(report)),
                Err(_) => AppEvent::RunDropped,
            },
            RunCompletion::Compaction(receiver) => match receiver.await {
                Ok(report) => AppEvent::CompactionFinished(Box::new(report)),
                Err(_) => AppEvent::RunDropped,
            },
            RunCompletion::HostedTurn(receiver) => match receiver.await {
                Ok(outcome) => AppEvent::HostedTurnFinished(Box::new(outcome)),
                Err(_) => AppEvent::RunDropped,
            },
            RunCompletion::Turn(receiver) => match receiver.await {
                Ok(outcome) => AppEvent::TurnFinished(Box::new(outcome)),
                Err(_) => AppEvent::RunDropped,
            },
        }
    }
}

pub struct SessionChange {
    pub session: Option<aicoder_core::session::RunSession>,
    pub info: Option<SessionInfo>,
    pub deleted: Option<String>,
    pub editor: Option<aicoder_core::session::ForkEditor>,
    pub notices: Vec<String>,
}
pub enum CapturedInput {
    Text(aicoder_runtime::references::PromptReference),
    Image(aicoder_runtime::images::ImageAttachment),
}
pub struct ReferenceJob {
    pub context: TurnExecutionContext,
    completed: oneshot::Receiver<Result<CapturedInput, String>>,
}
impl ReferenceJob {
    pub async fn recv(&mut self) -> AppEvent {
        AppEvent::ReferenceReady(Box::new(
            (&mut self.completed)
                .await
                .unwrap_or_else(|_| Err("Reference task ended without a result".into())),
        ))
    }
}
impl Drop for ReferenceJob {
    fn drop(&mut self) {
        self.context.cancel("Reference owner dropped");
    }
}
pub struct AgentRuntime {
    agent: Arc<Agent>,
    adopted_session: Arc<std::sync::Mutex<Option<String>>>,
    repository: Arc<JsonlSessionRepository>,
    workspace: PathBuf,
    runtime: Arc<aicoder_runtime::Runtime>,
    provider: Arc<dyn aicoder_core::ModelProvider>,
    approvals: ApprovalSessions,
    extensions: Option<Arc<aicoder_core::extensions::ExtensionSet>>,
}

impl AgentRuntime {
    pub fn theme(&self, selection: &str) -> Result<crate::theme::Theme> {
        crate::theme::Theme::select(&self.runtime, selection)
    }
    pub fn new(runtime: aicoder_runtime::Runtime) -> Result<Self> {
        let provider: Arc<dyn aicoder_core::ModelProvider> = Arc::new(runtime.session_client()?);
        Self::with_provider(runtime, provider, None)
    }
    pub fn with_provider(
        runtime: aicoder_runtime::Runtime,
        provider: Arc<dyn aicoder_core::ModelProvider>,
        extensions: Option<Arc<aicoder_core::extensions::ExtensionSet>>,
    ) -> Result<Self> {
        let repository = Arc::new(runtime.sessions()?);
        let builder = if let Some(extensions) = &extensions {
            runtime.executor_builder_with_extensions(provider.clone(), extensions.clone())?
        } else {
            runtime.executor_builder(provider.clone())?
        };
        let extensions = match (runtime.extensions(), extensions) {
            (Some(base), Some(extra)) => Some(Arc::new(base.combine(&extra)?)),
            (Some(base), None) => Some(base),
            (_, extra) => extra,
        };
        let executor = builder.build()?;
        let agent = Agent::new(executor, runtime.agent_config())
            .following_session_models()
            .following_session_reasoning();
        let agent = Arc::new(
            if !runtime.follows_session_reasoning()
                && runtime.config().options.reasoning_effort.is_none()
            {
                agent.recording_reasoning_defaults()
            } else {
                agent
            },
        );
        Ok(Self {
            agent,
            adopted_session: Arc::new(std::sync::Mutex::new(None)),
            repository,
            workspace: runtime.config().workspace.clone(),
            runtime: Arc::new(runtime),
            provider,
            approvals: ApprovalSessions::default(),
            extensions,
        })
    }

    pub fn commands(&self) -> Vec<aicoder_core::extensions::CommandSpec> {
        self.extensions
            .as_ref()
            .map(|e| e.commands())
            .unwrap_or_default()
    }
    pub fn start_command(
        &self,
        selection: SessionSelection,
        name: String,
        arguments: serde_json::Value,
        context: TurnExecutionContext,
    ) -> Result<ActiveRun> {
        let extensions = self
            .extensions
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Host has no registered extension commands"))?;
        let repository = self.repository.clone();
        let workspace = self.workspace.clone();
        let (sender, completion) = oneshot::channel();
        tokio::spawn(async move {
            let result = async {
                if context.is_cancelled() {
                    return Err(context.error().into());
                }
                let mut session = match selection {
                    SessionSelection::Existing(id) => Some(repository.open_run(&id)?),
                    _ => None,
                };
                anyhow::ensure!(
                    session
                        .as_ref()
                        .is_none_or(|s| s.metadata().cwd == workspace),
                    "foreign command session"
                );
                let _lease = session
                    .as_mut()
                    .map(|s| s.acquire(repository.as_ref()))
                    .transpose()?;
                extensions
                    .execute_command(
                        &name,
                        aicoder_core::extensions::CommandRequest {
                            context: aicoder_core::extensions::CommandContext {
                                workspace: &workspace,
                                session: session.as_ref().map(|s| s.metadata()),
                            },
                            arguments: &arguments,
                        },
                        &context,
                    )
                    .await
            }
            .await
            .map_err(|error: anyhow::Error| {
                aicoder_core::redaction::sanitize_text(&error.to_string(), 2048)
            });
            let _ = sender.send(result);
        });
        Ok(ActiveRun {
            events: None,
            completion: RunCompletion::Command(completion),
        })
    }
    pub fn start_reference(&self, path: String, context: TurnExecutionContext) -> ReferenceJob {
        let runtime = self.runtime.clone();
        let worker = context.clone();
        let (sent, completed) = oneshot::channel();
        tokio::spawn(async move {
            let result = runtime
                .read_reference(&path, worker)
                .await
                .map(CapturedInput::Text)
                .map_err(|error| aicoder_core::redaction::sanitize_text(&error.to_string(), 2048));
            let _ = sent.send(result);
        });
        ReferenceJob { context, completed }
    }
    pub fn start_image(&self, path: String, context: TurnExecutionContext) -> ReferenceJob {
        let runtime = self.runtime.clone();
        let worker = context.clone();
        let (sent, completed) = oneshot::channel();
        tokio::spawn(async move {
            let result = runtime
                .read_image(&path, Default::default(), worker)
                .await
                .map(CapturedInput::Image)
                .map_err(|e| aicoder_core::redaction::sanitize_text(&e.to_string(), 2048));
            let _ = sent.send(result);
        });
        ReferenceJob { context, completed }
    }
    pub fn has_input_hooks(&self) -> bool {
        self.agent.turn_executor().has_input_hooks()
    }
    pub fn supports_images(&self, selection: &SessionSelection) -> Result<bool> {
        let model = match selection {
            SessionSelection::Existing(id) => self
                .open_run_session(id)?
                .selected_model()
                .map(|(_, m)| m.to_string())
                .unwrap_or_else(|| self.runtime.config().provider.model.clone()),
            _ => self.runtime.config().provider.model.clone(),
        };
        Ok(self.provider.model_spec(&model)?.capabilities.images)
    }
    pub fn model_spec(&self, model: &str) -> Result<aicoder_core::model::ModelSpec> {
        Ok(self.provider.model_spec(model)?)
    }
    pub fn context_window(&self) -> Option<u64> {
        self.runtime
            .config()
            .context
            .window_tokens
            .map(|v| v as u64)
    }
    pub fn resource_summary(&self) -> Result<String> {
        let resources = self.runtime.resources()?;
        let manifest = resources.manifest();
        let files = manifest
            .files
            .iter()
            .filter(|file| file.active)
            .map(|file| format!("{:?}: {}", file.kind, file.path.display()))
            .collect::<Vec<_>>();
        Ok(format!(
            "Resource generation {}\n{}",
            manifest.generation,
            files.join("\n")
        ))
    }
    pub fn reload_resources(&mut self) -> Result<()> {
        let runtime = self.runtime.reload_resources()?;
        let builder = if let Some(extensions) = &self.extensions {
            // This field is the fully combined snapshot already adopted by the current host.
            runtime
                .executor_builder(self.provider.clone())?
                .extensions(extensions.clone())
        } else {
            runtime.executor_builder(self.provider.clone())?
        };
        let executor = builder.build()?;
        let agent = Agent::new(executor, runtime.agent_config())
            .following_session_models()
            .following_session_reasoning();
        let agent = Arc::new(
            if !runtime.follows_session_reasoning()
                && runtime.config().options.reasoning_effort.is_none()
            {
                agent.recording_reasoning_defaults()
            } else {
                agent
            },
        );
        self.approvals.clear();
        self.agent = Arc::new(
            Arc::try_unwrap(agent)
                .map_err(|_| anyhow::anyhow!("new agent unexpectedly shared"))?
                .with_custom_inbox(self.agent.custom_inbox())?,
        );
        self.runtime = Arc::new(runtime);
        Ok(())
    }
    pub fn sessions(&self) -> Result<Vec<SessionInfo>> {
        self.repository.list(&self.workspace)
    }

    pub fn open_run_session(&self, id: &str) -> Result<aicoder_core::session::RunSession> {
        aicoder_core::session::SessionNavigator::new(self.repository.as_ref(), &self.workspace)?
            .open_run(id)
    }
    pub fn navigate(
        &self,
        selection: &SessionSelection,
        command: crate::commands::SlashCommand,
        args: &str,
    ) -> Result<(Option<aicoder_core::session::RunSession>, String)> {
        use crate::commands::SlashCommand;
        let SessionSelection::Existing(id) = selection else {
            anyhow::bail!("Select a stored session first")
        };
        let nav = aicoder_core::session::SessionNavigator::new(
            self.repository.as_ref(),
            &self.workspace,
        )?;
        use aicoder_core::session::{ArchiveNavigation as Action, BranchSource};
        let action = match command {
            SlashCommand::Tree => {
                let cursor = if args.is_empty() {
                    None
                } else {
                    Some(serde_json::from_str(args)?)
                };
                return Ok((
                    None,
                    serde_json::to_string_pretty(&nav.tree(id, 50, cursor.as_ref())?)?,
                ));
            }
            SlashCommand::Search => {
                return Ok((
                    None,
                    serde_json::to_string_pretty(&nav.search(id, args, 20, None)?)?,
                ));
            }
            SlashCommand::SearchNext => {
                let cursor: aicoder_core::session::SearchCursor = serde_json::from_str(args)?;
                return Ok((
                    None,
                    serde_json::to_string_pretty(&nav.search(
                        id,
                        &cursor.query,
                        20,
                        Some(&cursor),
                    )?)?,
                ));
            }
            SlashCommand::Rename => Action::Rename { title: args.into() },
            SlashCommand::Label => Action::Label {
                entry_id: nav
                    .info(id)?
                    .leaf_id
                    .ok_or_else(|| anyhow::anyhow!("Empty root has no entry to label"))?,
                label: Some(args.into()),
            },
            SlashCommand::Unlabel => Action::Label {
                entry_id: args.into(),
                label: None,
            },
            SlashCommand::Checkout => Action::Checkout {
                target: (args != "root").then(|| args.into()),
            },
            SlashCommand::Branch => Action::Branch {
                source: if args == "root" {
                    BranchSource::Root
                } else if args.is_empty() {
                    BranchSource::Current
                } else {
                    BranchSource::Entry(args.into())
                },
                name: None,
            },
            SlashCommand::Recover => Action::Recover,
            _ => anyhow::bail!("Unsupported navigation command"),
        };
        anyhow::ensure!(
            !self.agent.has_session_hooks(),
            "Use asynchronous navigation when session hooks are installed"
        );
        let session = nav.execute(id, action)?;
        self.approvals.remove(&self.workspace, id);
        Ok((
            Some(session),
            "Session updated; previous branches remain available.".into(),
        ))
    }
    pub fn start_navigation(
        &self,
        selection: SessionSelection,
        command: crate::commands::SlashCommand,
        args: String,
        context: TurnExecutionContext,
    ) -> ActiveRun {
        let repository = self.repository.clone();
        let agent = self.agent.clone();
        let approvals = self.approvals.clone();
        let workspace = self.workspace.clone();
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let work = async {
                use crate::commands::SlashCommand as C;
                use aicoder_core::session::{ArchiveNavigation as A, BranchSource};
                let SessionSelection::Existing(id) = selection else {
                    anyhow::bail!("Select a stored session first")
                };
                let nav =
                    aicoder_core::session::SessionNavigator::new(repository.as_ref(), &workspace)?;
                let action = match command {
                    C::Rename => A::Rename { title: args },
                    C::Label => A::Label {
                        entry_id: nav
                            .info(&id)?
                            .leaf_id
                            .ok_or_else(|| anyhow::anyhow!("Empty root has no entry to label"))?,
                        label: Some(args),
                    },
                    C::Unlabel => A::Label {
                        entry_id: args,
                        label: None,
                    },
                    C::Checkout => A::Checkout {
                        target: (args != "root").then_some(args),
                    },
                    C::Branch => A::Branch {
                        source: if args == "root" {
                            BranchSource::Root
                        } else if args.is_empty() {
                            BranchSource::Current
                        } else {
                            BranchSource::Entry(args)
                        },
                        name: None,
                    },
                    C::Recover => A::Recover,
                    _ => anyhow::bail!("Unsupported navigation command"),
                };
                let output = agent
                    .navigate_session(repository.as_ref(), &id, action, context)
                    .await?;
                approvals.remove(&workspace, &id);
                let mut notices =
                    vec!["Session updated; previous branches remain available.".into()];
                if !output.report.delivery.failures.is_empty() {
                    notices.push(format!(
                        "Session change committed; {} lifecycle observations were incomplete.",
                        output.report.delivery.failures.len()
                    ));
                }
                let info = repository.session_info(&id).ok();
                Ok::<_, anyhow::Error>(SessionChange {
                    info,
                    deleted: None,
                    editor: None,
                    session: Some(output.session),
                    notices,
                })
            }
            .await
            .map_err(|e| aicoder_core::redaction::sanitize_text(&e.to_string(), 2048));
            let _ = sent.send(work);
        });
        ActiveRun {
            events: None,
            completion: RunCompletion::Navigation(received),
        }
    }
    pub fn has_session_hooks(&self) -> bool {
        self.agent.has_session_hooks()
    }
    pub fn start_session_selection(
        &self,
        current: SessionSelection,
        selection: SessionSelection,
        context: TurnExecutionContext,
    ) -> ActiveRun {
        let repository = self.repository.clone();
        let agent = self.agent.clone();
        let approvals = self.approvals.clone();
        let workspace = self.workspace.clone();
        let adopted = self.adopted_session.clone();
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let current = if agent.has_session_hooks() {
                adopted.lock().expect("TUI session binding").clone()
            } else {
                match current {
                    SessionSelection::Existing(id) => Some(id),
                    _ => None,
                }
            };
            let result = agent
                .select_session(repository.as_ref(), current.as_deref(), selection, context)
                .await
                .map(|output| {
                    let id = output.session.metadata().id.clone();
                    *adopted.lock().expect("TUI session binding") = Some(id.clone());
                    if current.as_deref() != Some(&id) {
                        if let Some(old) = &current {
                            approvals.remove(&workspace, old);
                        }
                        approvals.remove(&workspace, &id);
                    }
                    let failures = output
                        .reports
                        .iter()
                        .map(|r| r.delivery.failures.len())
                        .sum::<usize>();
                    let mut notices = vec!["Session selected.".into()];
                    if failures > 0 {
                        notices.push(format!(
                            "Session selected; {failures} lifecycle observations were incomplete."
                        ));
                    }
                    SessionChange {
                        deleted: None,
                        editor: None,
                        info: repository.session_info(&id).ok(),
                        session: Some(output.session),
                        notices,
                    }
                })
                .map_err(|e| aicoder_core::redaction::sanitize_text(&e.to_string(), 2048));
            let _ = sent.send(result);
        });
        ActiveRun {
            events: None,
            completion: RunCompletion::Navigation(received),
        }
    }
    pub fn start_delete_session(
        &self,
        selection: SessionSelection,
        id: String,
        context: TurnExecutionContext,
    ) -> ActiveRun {
        let repository = self.repository.clone();
        let agent = self.agent.clone();
        let approvals = self.approvals.clone();
        let workspace = self.workspace.clone();
        let adopted = self.adopted_session.clone();
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let work=async{
                let current=if agent.has_session_hooks(){adopted.lock().expect("TUI session binding").clone()}else{match selection{SessionSelection::Existing(id)=>Some(id),_=>None}};
                let nav=aicoder_core::session::SessionNavigator::new(repository.as_ref(),&workspace)?;nav.info(&id)?;
                let mut session=None;let mut info=None;let mut notices=Vec::new();
                if current.as_deref()==Some(&id){
                    let output=agent.select_session(repository.as_ref(),Some(&id),SessionSelection::New,context.clone()).await?;
                    let selected=output.session.metadata().id.clone();*adopted.lock().expect("TUI session binding")=Some(selected.clone());
                    approvals.remove(&workspace,&id);info=repository.session_info(&selected).ok();session=Some(output.session);
                    if output.reports.iter().any(|r|!r.delivery.failures.is_empty()){notices.push("Session selected; lifecycle observations were incomplete.".into());}
                }
                let deleted=if context.is_cancelled(){notices.push("Deletion cancelled; the session remains stored.".into());None}
                else{match repository.delete(&id){Ok(())=>{approvals.remove(&workspace,&id);notices.push("Session deleted.".into());Some(id)},Err(error)=>{if session.is_none(){return Err(error)}notices.push("New session selected, but the previous session could not be deleted; inspect it before retrying.".into());None}}};
                Ok::<_,anyhow::Error>(SessionChange{session,info,deleted,editor:None,notices})
            }.await.map_err(|e|aicoder_core::redaction::sanitize_text(&e.to_string(),2048));
            let _ = sent.send(work);
        });
        ActiveRun {
            events: None,
            completion: RunCompletion::Navigation(received),
        }
    }
    pub fn start_fork(
        &self,
        selection: SessionSelection,
        point: aicoder_core::session::ForkPoint,
        context: TurnExecutionContext,
    ) -> ActiveRun {
        let agent = self.agent.clone();
        let repository = self.repository.clone();
        let adopted = self.adopted_session.clone();
        let approvals = self.approvals.clone();
        let workspace = self.workspace.clone();
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let work = async {
                let SessionSelection::Existing(id) = selection else {
                    anyhow::bail!("Select a stored session first")
                };
                let output = agent
                    .fork_session(
                        repository.as_ref(),
                        &id,
                        point,
                        aicoder_core::session::SessionForkLimits::default(),
                        context,
                    )
                    .await?;
                let target = output.session.metadata().id.clone();
                *adopted.lock().expect("TUI session binding") = Some(target.clone());
                approvals.remove(&workspace, &id);
                approvals.remove(&workspace, &target);
                let mut notices = vec![format!("New forked session: {target}")];
                if output
                    .reports
                    .iter()
                    .any(|r| !r.delivery.failures.is_empty())
                {
                    notices.push("Fork published; lifecycle observations were incomplete.".into());
                }
                Ok::<_, anyhow::Error>(SessionChange {
                    info: repository.session_info(&target).ok(),
                    session: Some(output.session),
                    deleted: None,
                    editor: output.editor,
                    notices,
                })
            }
            .await
            .map_err(|e| aicoder_core::redaction::sanitize_text(&e.to_string(), 2048));
            let _ = sent.send(work);
        });
        ActiveRun {
            events: None,
            completion: RunCompletion::Navigation(received),
        }
    }
    pub fn start_reload_notifications(&self, context: TurnExecutionContext) -> ActiveRun {
        let agent = self.agent.clone();
        let repository = self.repository.clone();
        let selected = self
            .adopted_session
            .lock()
            .expect("TUI session binding")
            .clone();
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let mut notices = vec!["Resources reloaded for the next run.".into()];
            if let Some(id) = selected {
                let reports = agent
                    .notify_session_reload(repository.as_ref(), &id, context)
                    .await;
                if reports.as_ref().is_err()
                    || reports
                        .as_ref()
                        .is_ok_and(|r| r.iter().any(|e| !e.delivery.failures.is_empty()))
                {
                    notices
                        .push("Resources reloaded; lifecycle observations were incomplete.".into());
                }
            }
            let _ = sent.send(Ok(SessionChange {
                session: None,
                info: None,
                deleted: None,
                editor: None,
                notices,
            }));
        });
        ActiveRun {
            events: None,
            completion: RunCompletion::Navigation(received),
        }
    }
    pub async fn notify_shutdown(&self, _selection: &SessionSelection) -> Result<()> {
        self.approvals.clear();
        let selected = self
            .adopted_session
            .lock()
            .expect("TUI session binding")
            .take();
        if self.has_session_hooks()
            && let Some(id) = selected
        {
            let report = self
                .agent
                .notify_session_shutdown(
                    self.repository.as_ref(),
                    &id,
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
        Ok(())
    }
    pub async fn select_model(
        &self,
        id: &str,
        model: &str,
        context: TurnExecutionContext,
    ) -> Result<aicoder_core::session::ModelSelectionReport> {
        let report = self
            .agent
            .switch_session_model(self.repository.as_ref(), id, model, context)
            .await?;
        self.approvals.remove(&self.workspace, id);
        Ok(report)
    }
    pub fn start_model_selection(
        &self,
        id: String,
        model: String,
        context: TurnExecutionContext,
    ) -> ActiveRun {
        let agent = self.agent.clone();
        let repository = self.repository.clone();
        let approvals = self.approvals.clone();
        let workspace = self.workspace.clone();
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let report = agent
                .switch_session_model(repository.as_ref(), &id, &model, context)
                .await
                .map_err(|error| aicoder_core::redaction::sanitize_text(&error.to_string(), 2048));
            if report.is_ok() {
                approvals.remove(&workspace, &id);
            }
            let _ = sent.send(report);
        });
        ActiveRun {
            events: None,
            completion: RunCompletion::Model(received),
        }
    }
    pub async fn select_reasoning(
        &self,
        id: &str,
        effort: Option<aicoder_core::model::ReasoningEffort>,
        context: TurnExecutionContext,
    ) -> Result<aicoder_core::session::ModelSelectionReport> {
        let report = self
            .agent
            .switch_session_reasoning(self.repository.as_ref(), id, effort, context)
            .await?;
        self.approvals.remove(&self.workspace, id);
        Ok(report)
    }
    pub fn reasoning_summary(
        &self,
        selection: &SessionSelection,
        pending: Option<Option<aicoder_core::model::ReasoningEffort>>,
    ) -> Result<String> {
        let session = match selection {
            SessionSelection::Existing(id) => Some(self.open_run_session(id)?),
            _ => None,
        };
        let model = session
            .as_ref()
            .and_then(|s| s.selected_model().map(|(_, m)| m.to_string()))
            .unwrap_or_else(|| self.runtime.config().provider.model.clone());
        let spec = self.provider.model_spec(&model)?;
        let choice = session
            .as_ref()
            .map(|s| s.reasoning_selection())
            .transpose()?
            .flatten();
        let effort = pending.unwrap_or_else(|| {
            choice
                .map(|c| c.effort)
                .unwrap_or(self.runtime.config().options.reasoning_effort)
        });
        let supported = spec
            .capabilities
            .reasoning_efforts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "Reasoning: {}; declared levels: {}. Use /thinking LEVEL or /thinking default.",
            effort.map_or("service/host default".into(), |v| v.to_string()),
            if supported.is_empty() {
                "no declared controls"
            } else {
                &supported
            }
        ))
    }
    pub fn start_reasoning_selection(
        &self,
        selection: SessionSelection,
        effort: Option<aicoder_core::model::ReasoningEffort>,
        context: TurnExecutionContext,
    ) -> ActiveRun {
        let agent = self.agent.clone();
        let repository = self.repository.clone();
        let approvals = self.approvals.clone();
        let workspace = self.workspace.clone();
        let config = self.runtime.agent_config();
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let report = async {
                match selection {
                    SessionSelection::Existing(id) => {
                        let report = agent
                            .switch_session_reasoning(
                                repository.as_ref(),
                                &id,
                                effort,
                                context.clone(),
                            )
                            .await?;
                        approvals.remove(&workspace, &id);
                        Ok(report)
                    }
                    _ => {
                        let mut request = aicoder_core::model::ModelRequest::new(
                            config.model,
                            config
                                .system_prompt
                                .map(|p| {
                                    aicoder_core::model::Message::text(
                                        aicoder_core::model::MessageRole::System,
                                        p,
                                    )
                                })
                                .into_iter()
                                .collect(),
                        );
                        request.options = config.options;
                        request.options.reasoning_effort = effort;
                        let input = agent
                            .turn_executor()
                            .prepare_input(
                                aicoder_core::ExecutionInput::from_request(request),
                                &context,
                            )
                            .await?;
                        let report = agent.turn_executor().validate_model_selection(&input)?;
                        if context.is_cancelled() {
                            return Err(context.error().into());
                        }
                        Ok::<_, anyhow::Error>(report)
                    }
                }
            }
            .await
            .map_err(|error| aicoder_core::redaction::sanitize_text(&error.to_string(), 2048));
            let _ = sent.send(report);
        });
        ActiveRun {
            events: None,
            completion: RunCompletion::Model(received),
        }
    }
    pub fn control_channel(
        &self,
        context: TurnExecutionContext,
        config: aicoder_core::RunControlConfig,
    ) -> Result<(aicoder_core::RunHandle, RunControl), aicoder_core::RunControlError> {
        self.agent.control_channel(context, config)
    }
    pub fn execution_context(&self) -> TurnExecutionContext {
        TurnExecutionContext::new().with_approval_sessions(self.approvals.clone())
    }
    pub fn permissions(&self, selection: &SessionSelection) -> Option<ApprovalSnapshot> {
        match selection {
            SessionSelection::Existing(id) => self
                .approvals
                .get(&self.workspace, id)
                .map(|session| session.snapshot()),
            _ => None,
        }
    }
    pub fn revoke_permissions(&self, selection: &SessionSelection) -> bool {
        if let SessionSelection::Existing(id) = selection
            && let Some(session) = self.approvals.get(&self.workspace, id)
        {
            session.revoke_all();
            return true;
        }
        false
    }
    pub fn start_compaction(&self, id: String, context: TurnExecutionContext) -> Result<ActiveRun> {
        let agent = match &self.extensions {
            Some(extensions) => self
                .runtime
                .summary_agent_with_extensions(self.provider.clone(), extensions.clone())?,
            None => self.runtime.summary_agent(self.provider.clone())?,
        };
        let repository = self.repository.clone();
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let report = agent
                .compact_session(repository.as_ref(), &id, context)
                .await;
            let _ = sent.send(report);
        });
        Ok(ActiveRun {
            events: None,
            completion: RunCompletion::Compaction(received),
        })
    }
    pub fn start_turn(
        &self,
        selection: SessionSelection,
        prompt: aicoder_core::AgentPrompt,
        reasoning: Option<Option<aicoder_core::model::ReasoningEffort>>,
        control: RunControl,
    ) -> Result<ActiveRun> {
        let events = control.subscribe(EventBufferConfig::default())?;
        let agent = reasoning.map_or_else(
            || self.agent.clone(),
            |effort| Arc::new(self.agent.with_reasoning_effort(effort)),
        );
        let repository = self.repository.clone();
        if agent.has_session_hooks() {
            let current = self
                .adopted_session
                .lock()
                .expect("TUI session binding")
                .clone();
            let adopted = self.adopted_session.clone();
            let (sender, completion) = oneshot::channel();
            tokio::spawn(async move {
                let hosted = agent
                    .run_hosted_session_controlled(
                        repository.as_ref(),
                        current.as_deref(),
                        selection,
                        prompt,
                        Arc::new(()),
                        control,
                    )
                    .await;
                if let Some(session) = hosted.outcome.session() {
                    *adopted.lock().expect("TUI session binding") = Some(session.id.clone());
                }
                let _ = sender.send(hosted);
            });
            return Ok(ActiveRun {
                events: Some(events),
                completion: RunCompletion::HostedTurn(completion),
            });
        }
        let (sender, completion) = oneshot::channel();
        tokio::spawn(async move {
            let outcome = agent
                .run_prompt_with_session_controlled(
                    repository.as_ref(),
                    selection,
                    prompt,
                    Arc::new(()),
                    control,
                )
                .await;
            let _ = sender.send(outcome);
        });
        Ok(ActiveRun {
            events: Some(events),
            completion: RunCompletion::Turn(completion),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicoder_core::{
        AgentRawEvent, AgentTurnResult, RunControlStatus, RunHandle, RunProgress,
        TurnExecutionResult,
        events::{AgentEventEmitter, EventBufferConfig},
        model::{Message, MessageRole, StopReason},
    };
    #[tokio::test]
    async fn incomplete_observation_is_reported_before_the_authoritative_result() {
        let events = AgentEventEmitter::new(Arc::new(()));
        let stream = events
            .source()
            .subscribe(EventBufferConfig {
                max_events: 1,
                max_bytes: 4096,
            })
            .unwrap();
        for _ in 0..3 {
            events.emit(AgentRawEvent::ContentChunk {
                response_index: 0,
                delta: "part".into(),
            });
        }
        events.shutdown().await;
        let (sender, completion) = oneshot::channel();
        sender
            .send(AgentTurnOutcome::Completed(AgentTurnResult {
                execution_result: TurnExecutionResult {
                    final_message: Message::text(MessageRole::Assistant, "full result"),
                    finish_reason: Some(StopReason::Complete),
                    progress: RunProgress::new(vec![]),
                },
                session: None,
            }))
            .unwrap();
        let mut active = ActiveRun {
            events: Some(stream),
            completion: RunCompletion::Turn(completion),
        };
        assert!(matches!(active.recv().await,AppEvent::Agent(event) if event.sequence==1));
        assert!(matches!(
            active.recv().await,
            AppEvent::ObservationFailed(_)
        ));
        assert!(matches!(active.recv().await, AppEvent::TurnFinished(_)));
    }
    #[tokio::test]
    async fn dropping_the_ui_observation_does_not_cancel_the_execution_owner() {
        let (handle, control) = RunHandle::pair();
        let events = control.subscribe(EventBufferConfig::default()).unwrap();
        let (_sender, completion) = oneshot::channel();
        drop(ActiveRun {
            events: Some(events),
            completion: RunCompletion::Turn(completion),
        });
        assert_eq!(handle.snapshot().status, RunControlStatus::Waiting);
        assert!(
            handle
                .steer(Message::text(MessageRole::User, "still available"))
                .is_ok()
        );
        drop(control);
    }
    #[tokio::test]
    async fn resource_reload_and_session_deletion_invalidate_their_authorization_handles() {
        use aicoder_runtime::{
            ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings,
            resources::ResourceSettings,
        };
        use std::collections::BTreeMap;
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let application = Runtime::load_with_environment(
            workspace.path(),
            RuntimeOverrides {
                settings: Settings {
                    resources: ResourceSettings {
                        parents: Setting::Value(false),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
            ConfigDiscovery::default(),
            Arc::new(BTreeMap::from([
                (
                    "AICODER_HOME".into(),
                    root.path().to_string_lossy().to_string(),
                ),
                ("AICODER_PROXY".into(), "disabled".into()),
            ])),
        )
        .unwrap();
        let mut runtime = AgentRuntime::new(application).unwrap();
        let session = runtime.repository.create(&runtime.workspace).unwrap();
        let id = session.metadata().id.clone();
        let scope = runtime.approvals.session(&runtime.workspace, &id).unwrap();
        std::fs::write(
            workspace.path().join("AGENTS.md"),
            "x".repeat(2 * 1024 * 1024),
        )
        .unwrap();
        assert!(runtime.reload_resources().is_err());
        assert!(!scope.snapshot().session.closed);
        std::fs::remove_file(workspace.path().join("AGENTS.md")).unwrap();
        runtime.reload_resources().unwrap();
        assert!(scope.snapshot().session.closed);
        let scope = runtime.approvals.session(&runtime.workspace, &id).unwrap();
        let mut deleting = runtime.start_delete_session(
            SessionSelection::Existing(id.clone()),
            id.clone(),
            runtime.execution_context(),
        );
        assert!(matches!(deleting.recv().await,AppEvent::SessionChanged(result) if result.is_ok()));
        assert!(scope.snapshot().session.closed);
        assert!(
            runtime
                .permissions(&SessionSelection::Existing(id))
                .is_none()
        );
    }
    #[tokio::test]
    async fn navigation_and_model_selection_share_core_state_and_retire_old_grants() {
        use crate::commands::SlashCommand;
        use aicoder_runtime::{
            ConfigDiscovery, Runtime, RuntimeOverrides, Setting, Settings,
            resources::ResourceSettings,
        };
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let runtime = Runtime::load_with_environment(
            workspace.path(),
            RuntimeOverrides {
                settings: Settings {
                    resources: ResourceSettings {
                        enabled: Setting::Value(false),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
            ConfigDiscovery {
                enabled: false,
                ..Default::default()
            },
            Arc::new(std::collections::BTreeMap::from([(
                "AICODER_HOME".into(),
                state.path().to_string_lossy().into_owned(),
            )])),
        )
        .unwrap();
        let runtime = AgentRuntime::new(runtime).unwrap();
        let mut session = runtime.repository.create(workspace.path()).unwrap();
        runtime
            .repository
            .append(
                &mut session,
                aicoder_core::model::Message::text(
                    aicoder_core::model::MessageRole::User,
                    "needle",
                ),
            )
            .unwrap();
        let id = session.metadata().id.clone();
        let selection = SessionSelection::Existing(id.clone());
        let scope = runtime.approvals.session(&runtime.workspace, &id).unwrap();
        let (view, notice) = runtime
            .navigate(&selection, SlashCommand::Tree, "")
            .unwrap();
        assert!(view.is_none() && notice.contains("rows"));
        assert!(!scope.snapshot().session.closed);
        let (renamed, _) = runtime
            .navigate(&selection, SlashCommand::Rename, "ui title")
            .unwrap();
        assert_eq!(
            renamed.unwrap().metadata().title.as_deref(),
            Some("ui title")
        );
        assert!(scope.snapshot().session.closed);
        let scope = runtime.approvals.session(&runtime.workspace, &id).unwrap();
        let mut operation = runtime.start_model_selection(
            id.clone(),
            "another-model".into(),
            runtime.execution_context(),
        );
        let result = operation.recv().await;
        assert!(matches!(result,AppEvent::ModelSelected(report) if report.is_ok()));
        assert!(scope.snapshot().session.closed);
        assert_eq!(
            runtime
                .repository
                .open(&id)
                .unwrap()
                .selected_model()
                .unwrap()
                .1,
            "another-model"
        );
    }
}
