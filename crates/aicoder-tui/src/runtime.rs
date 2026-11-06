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
    Compaction(oneshot::Receiver<aicoder_core::summary::ManualCompactionReport>),

    Model(oneshot::Receiver<Result<aicoder_core::session::ModelSelectionReport, String>>),
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
            RunCompletion::Model(receiver) => match receiver.await {
                Ok(report) => AppEvent::ModelSelected(Box::new(report)),
                Err(_) => AppEvent::RunDropped,
            },
            RunCompletion::Compaction(receiver) => match receiver.await {
                Ok(report) => AppEvent::CompactionFinished(Box::new(report)),
                Err(_) => AppEvent::RunDropped,
            },
            RunCompletion::Turn(receiver) => match receiver.await {
                Ok(outcome) => AppEvent::TurnFinished(Box::new(outcome)),
                Err(_) => AppEvent::RunDropped,
            },
        }
    }
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
    pub fn delete_session(&self, id: &str) -> Result<()> {
        aicoder_core::session::SessionNavigator::new(self.repository.as_ref(), &self.workspace)?
            .info(id)?;
        self.repository.delete(id)?;
        self.approvals.remove(&self.workspace, id);
        Ok(())
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
        let session = nav.execute(id, action)?;
        self.approvals.remove(&self.workspace, id);
        Ok((
            Some(session),
            "Session updated; previous branches remain available.".into(),
        ))
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
    #[test]
    fn resource_reload_and_session_deletion_invalidate_their_authorization_handles() {
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
        runtime.delete_session(&id).unwrap();
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
