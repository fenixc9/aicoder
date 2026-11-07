//! Native navigation guards around read-only owned plans and repository leases.
use super::*;
use crate::{Agent, ExecutionInput, TurnExecutionContext, extensions::*};
use anyhow::Result;
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionActionReport {
    pub event: SessionLifecycleEvent,
    pub delivery: SessionLifecycleDelivery,
}
pub struct SessionNavigationOutcome {
    pub session: RunSession,
    pub report: SessionActionReport,
}
impl Agent {
    pub(crate) async fn session_before(
        &self,
        operation: &SessionOperation,
        source: Option<&SessionPosition>,
        target: Option<&SessionPosition>,
        context: &TurnExecutionContext,
    ) -> Result<()> {
        if let Some(hooks) = &self.turn_executor().session_hooks {
            let work = hooks.before(
                self.turn_executor().workspace_root(),
                operation,
                source,
                target,
                context,
            );
            if let Some(duration) = self.turn_executor().execution_config().budget.max_duration {
                let started = context.preparation_start(tokio::time::Instant::now());
                let deadline = started
                    .checked_add(duration)
                    .ok_or_else(|| anyhow::anyhow!("session operation duration is too large"))?;
                tokio::select! {biased;_=tokio::time::sleep_until(deadline)=>return Err(self.turn_executor().preparation_budget_error(started).into()),result=work=>result?}
            } else {
                work.await?;
            }
        }
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        Ok(())
    }
    pub(crate) async fn session_after(
        &self,
        event: SessionLifecycleEvent,
        context: &TurnExecutionContext,
    ) -> SessionActionReport {
        let delivery = match &self.turn_executor().session_hooks {
            Some(hooks) => {
                let deadline = self
                    .turn_executor()
                    .execution_config()
                    .budget
                    .max_duration
                    .and_then(|d| {
                        context
                            .preparation_start(tokio::time::Instant::now())
                            .checked_add(d)
                    });
                hooks.after(&event, context, deadline).await
            }
            None => SessionLifecycleDelivery::default(),
        };
        SessionActionReport { event, delivery }
    }
    pub async fn navigate_session(
        &self,
        repository: &dyn SessionRepository,
        id: &str,
        action: ArchiveNavigation,
        context: TurnExecutionContext,
    ) -> Result<SessionNavigationOutcome> {
        let context = context
            .child()
            .with_preparation_start(tokio::time::Instant::now());
        let navigator = SessionNavigator::new(repository, self.turn_executor().workspace_root())?;
        let prepared = navigator.prepare(id, action)?;
        self.apply_navigation(prepared, context).await
    }
    /// Adopt exactly this owned read-only plan after validation and native guards.
    pub async fn apply_navigation(
        &self,
        prepared: PreparedSessionNavigation<'_>,
        context: TurnExecutionContext,
    ) -> Result<SessionNavigationOutcome> {
        let context = context
            .child()
            .with_preparation_start(tokio::time::Instant::now());
        anyhow::ensure!(
            prepared.plan().source().cwd == self.turn_executor().workspace_root(),
            "prepared navigation belongs to another workspace"
        );
        let plan = prepared.plan();
        let source = SessionPosition::from(plan.source());
        let target = SessionPosition::from(plan.target());
        let operation = SessionOperation::Navigation {
            action: plan.action().clone(),
        };
        let input = if matches!(
            plan.action(),
            ArchiveNavigation::Checkout { .. } | ArchiveNavigation::Branch { .. }
        ) {
            Some(self.prepare_session_view(plan.preview(), &context).await?)
        } else {
            None
        };
        self.session_before(&operation, Some(&source), Some(&target), &context)
            .await?;
        if let Some(input) = &input {
            self.turn_executor().validate_model_selection(input)?;
            self.turn_executor().check_preparation_budget(input)?;
        }
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let kind = if matches!(
            plan.action(),
            ArchiveNavigation::Rename { .. } | ArchiveNavigation::Label { .. }
        ) {
            SessionEventKind::InfoChanged
        } else {
            SessionEventKind::Navigated
        };
        let (session, lease) = prepared.commit_owned()?;
        if let Some(authority) = &context.approval_sessions {
            authority.remove(
                self.turn_executor().workspace_root(),
                &session.metadata().id,
            );
        }
        let event = SessionLifecycleEvent {
            reason: None,
            kind,
            operation,
            source: Some(source),
            target: Some(SessionPosition::from(session.metadata())),
        };
        let report = self.session_after(event, &context).await;
        drop(lease);
        Ok(SessionNavigationOutcome { session, report })
    }
    pub(crate) async fn prepare_session_view(
        &self,
        session: &RunSession,
        context: &TurnExecutionContext,
    ) -> Result<ExecutionInput> {
        let mut request = self.completion_request(self.system_messages());
        request.model = self.selected_model(session)?;
        self.apply_session_reasoning(&mut request, session)?;
        let input = ExecutionInput::from_run_session(request, session)?;
        let input = self.turn_executor().prepare_input(input, context).await?;
        self.turn_executor().validate_model_selection(&input)?;
        Ok(input)
    }
}

pub struct SessionSelectionOutcome {
    pub session: RunSession,
    pub reports: Vec<SessionActionReport>,
}
#[derive(Debug, Clone)]
pub struct SessionCreatedWithoutOwnership {
    pub created: SessionMetadata,
}
impl std::fmt::Display for SessionCreatedWithoutOwnership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "session {} was created but ownership was unavailable; inspect it before requesting another new session",
            self.created.id
        )
    }
}
impl std::error::Error for SessionCreatedWithoutOwnership {}
impl Agent {
    /// Explicit host adoption. Ordinary turns on Existing do not implicitly repeat this lifecycle.
    pub async fn select_session(
        &self,
        repository: &dyn SessionRepository,
        current: Option<&str>,
        selection: crate::SessionSelection,
        context: TurnExecutionContext,
    ) -> Result<SessionSelectionOutcome> {
        let context = context
            .child()
            .with_preparation_start(tokio::time::Instant::now());
        let nav = SessionNavigator::new(repository, self.turn_executor().workspace_root())?;
        let mut source = current.map(|id| nav.open_run(id)).transpose()?;
        let _source_lease = source.as_mut().map(|s| s.acquire(repository)).transpose()?;
        let source_position = source.as_ref().map(|s| SessionPosition::from(s.metadata()));
        let target_id = match selection {
            crate::SessionSelection::Existing(id) => Some(id),
            crate::SessionSelection::New => None,
            crate::SessionSelection::ContinueMostRecent => repository
                .most_recent(self.turn_executor().workspace_root())?
                .map(|s| s.id),
        };
        if target_id.as_deref() == current && current.is_some() {
            let mut session = source.expect("selected source");
            session.clear_run_token();
            return Ok(SessionSelectionOutcome {
                session,
                reports: Vec::new(),
            });
        }
        let mut target = target_id
            .as_deref()
            .map(|id| nav.open_run(id))
            .transpose()?;
        let mut target_lease = target.as_mut().map(|s| s.acquire(repository)).transpose()?;
        let target_position = target.as_ref().map(|s| SessionPosition::from(s.metadata()));
        let operation = match &target_id {
            Some(id) => SessionOperation::Resume {
                session_id: id.clone(),
            },
            None => SessionOperation::New,
        };
        let input = if let Some(target) = &target {
            self.prepare_session_view(target, &context).await?
        } else {
            let request = self.completion_request(self.system_messages());
            let input = self
                .turn_executor()
                .prepare_input(ExecutionInput::from_request(request), &context)
                .await?;
            self.turn_executor().validate_model_selection(&input)?;
            input
        };
        self.session_before(
            &operation,
            source_position.as_ref(),
            target_position.as_ref(),
            &context,
        )
        .await?;
        self.turn_executor().validate_model_selection(&input)?;
        self.turn_executor().check_preparation_budget(&input)?;
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        if target.is_none() {
            let mut created = RunSession::Full(Box::new(
                repository.create(self.turn_executor().workspace_root())?,
            ));
            target_lease =
                Some(
                    created
                        .acquire(repository)
                        .map_err(|_| SessionCreatedWithoutOwnership {
                            created: created.metadata().clone(),
                        })?,
                );
            target = Some(created);
        }
        let mut session = target.expect("selected session");
        if let Some(authority) = &context.approval_sessions {
            if let Some(old) = &source_position {
                authority.remove(self.turn_executor().workspace_root(), &old.id);
            }
            authority.remove(
                self.turn_executor().workspace_root(),
                &session.metadata().id,
            );
        }
        let target_position = Some(SessionPosition::from(session.metadata()));
        let reason = if source_position.is_none() {
            SessionLifecycleReason::Startup
        } else if target_id.is_none() {
            SessionLifecycleReason::New
        } else {
            SessionLifecycleReason::Resume
        };
        let mut reports = Vec::new();
        if source_position.is_some() {
            reports.push(
                self.session_after(
                    SessionLifecycleEvent {
                        kind: SessionEventKind::Shutdown,
                        reason: Some(reason.clone()),
                        operation: operation.clone(),
                        source: source_position.clone(),
                        target: target_position.clone(),
                    },
                    &context,
                )
                .await,
            );
        }
        reports.push(
            self.session_after(
                SessionLifecycleEvent {
                    kind: SessionEventKind::Started,
                    reason: Some(reason),
                    operation,
                    source: source_position,
                    target: target_position,
                },
                &context,
            )
            .await,
        );
        drop(target_lease);
        session.clear_run_token();
        Ok(SessionSelectionOutcome { session, reports })
    }
    /// Call only after the execution owner has settled. Shutdown is observational, never a veto.
    pub async fn notify_session_shutdown(
        &self,
        repository: &dyn SessionRepository,
        id: &str,
        reason: SessionShutdownReason,
        context: TurnExecutionContext,
    ) -> Result<SessionActionReport> {
        let nav = SessionNavigator::new(repository, self.turn_executor().workspace_root())?;
        let mut session = nav.open_run(id)?;
        let _lease = session.acquire(repository)?;
        let event_reason = match reason {
            SessionShutdownReason::Fork => SessionLifecycleReason::Fork,
            SessionShutdownReason::Quit => SessionLifecycleReason::Quit,
            SessionShutdownReason::Reload => SessionLifecycleReason::Reload,
            SessionShutdownReason::New => SessionLifecycleReason::New,
            SessionShutdownReason::Resume => SessionLifecycleReason::Resume,
        };
        Ok(self
            .session_after(
                SessionLifecycleEvent {
                    kind: SessionEventKind::Shutdown,
                    reason: Some(event_reason),
                    operation: SessionOperation::Shutdown { reason },
                    source: Some(SessionPosition::from(session.metadata())),
                    target: None,
                },
                &context,
            )
            .await)
    }
    /// Notification after a host has adopted its new immutable runtime generation.
    pub async fn notify_session_reload(
        &self,
        repository: &dyn SessionRepository,
        id: &str,
        context: TurnExecutionContext,
    ) -> Result<Vec<SessionActionReport>> {
        let nav = SessionNavigator::new(repository, self.turn_executor().workspace_root())?;
        let mut session = nav.open_run(id)?;
        let _lease = session.acquire(repository)?;
        let position = Some(SessionPosition::from(session.metadata()));
        let mut reports = Vec::new();
        for kind in [SessionEventKind::Shutdown, SessionEventKind::Started] {
            reports.push(
                self.session_after(
                    SessionLifecycleEvent {
                        kind,
                        reason: Some(SessionLifecycleReason::Reload),
                        operation: SessionOperation::Reload,
                        source: position.clone(),
                        target: position.clone(),
                    },
                    &context,
                )
                .await,
            );
        }
        Ok(reports)
    }
}
