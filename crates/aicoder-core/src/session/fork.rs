//! Bounded session branching into a new identity. Plans carry no serialized IO authority.
use super::*;
use crate::{TurnExecutionContext, model::*};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{any::Any, collections::BTreeMap, sync::Arc};
pub const FORK_NAMESPACE: &str = "aicoder.session.fork.v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "position",
    content = "entry",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ForkPoint {
    Current,
    BeforeLastUser,
    Root,
    Before(String),
    At(String),
}
#[derive(Debug, Clone, Copy)]
pub struct SessionForkLimits {
    pub max_entries: usize,
    pub max_bytes: usize,
    pub max_source_entries: usize,
    pub max_source_bytes: u64,
    pub max_editor_bytes: usize,
}
impl Default for SessionForkLimits {
    fn default() -> Self {
        Self {
            max_entries: 10000,
            max_bytes: 16 * 1024 * 1024,
            max_source_entries: 100000,
            max_source_bytes: 128 * 1024 * 1024,
            max_editor_bytes: 64 * 1024,
        }
    }
}
impl SessionForkLimits {
    pub fn validate(self) -> Result<Self> {
        ensure!(
            (1..=100000).contains(&self.max_entries)
                && (1024..=64 * 1024 * 1024).contains(&self.max_bytes)
                && self.max_source_entries >= self.max_entries
                && self.max_source_entries <= 1000000
                && self.max_source_bytes >= self.max_bytes as u64
                && self.max_source_bytes <= 1024 * 1024 * 1024
                && (1..=1024 * 1024).contains(&self.max_editor_bytes),
            "invalid session fork limits"
        );
        Ok(self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkEditor {
    pub source_entry_id: String,
    pub text: String,
    pub text_complete: bool,
    pub image_blocks_omitted: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkProvenance {
    pub source_session_id: String,
    pub source_revision: u64,
    pub source_selected_leaf: Option<String>,
    pub copied_leaf: Option<String>,
    pub point: ForkPoint,
    pub copied_entries: usize,
    pub navigation_records_archived: usize,
    pub source_ignored_tail_bytes: u64,
}
/// A prepared target snapshot. Backend pins remain immutable native host objects.
pub struct SessionForkPlan {
    source: SessionMetadata,
    point: ForkPoint,
    target: RunSession,
    editor: Option<ForkEditor>,
    provenance: ForkProvenance,
    limits: SessionForkLimits,
    backend: Option<Arc<dyn Any + Send + Sync>>,
    window_preview: Option<SessionWindow>,
    timing: Option<(tokio::time::Instant, std::time::Duration)>,
}
impl SessionForkPlan {
    pub fn source(&self) -> &SessionMetadata {
        &self.source
    }
    pub fn point(&self) -> &ForkPoint {
        &self.point
    }
    pub fn target(&self) -> &SessionMetadata {
        self.target.metadata()
    }
    pub fn editor(&self) -> Option<&ForkEditor> {
        self.editor.as_ref()
    }
    pub fn provenance(&self) -> &ForkProvenance {
        &self.provenance
    }
    pub fn limits(&self) -> SessionForkLimits {
        self.limits
    }
    pub fn snapshot(&self) -> &Session {
        match &self.target {
            RunSession::Full(s) => s,
            RunSession::Window(_) => unreachable!("fork plans hold complete admitted snapshots"),
        }
    }
    pub(crate) fn preview(&self) -> RunSession {
        self.window_preview.as_ref().map_or_else(
            || RunSession::Full(Box::new(self.snapshot().clone())),
            |w| RunSession::Window(Box::new(w.clone())),
        )
    }
    pub fn set_window_preview(&mut self, window: SessionWindow) -> Result<()> {
        ensure!(
            window.metadata() == self.target(),
            "fork window preview metadata mismatch"
        );
        self.window_preview = Some(window);
        Ok(())
    }
    pub fn window_preview(&self) -> Option<&SessionWindow> {
        self.window_preview.as_ref()
    }
    pub fn attach_backend<T: Any + Send + Sync>(&mut self, backend: T) {
        self.backend = Some(Arc::new(backend));
    }
    pub fn backend<T: Any>(&self) -> Option<&T> {
        self.backend.as_ref()?.downcast_ref()
    }
    pub fn set_publication_revision(&mut self, revision: u64) -> Result<()> {
        ensure!(revision > 0, "fork publication must contain its provenance");
        let mut metadata = self.target().clone();
        metadata.revision = revision;
        self.target = RunSession::Full(Box::new(Session::from_entries(
            metadata,
            self.snapshot().entries().to_vec(),
        )?));
        Ok(())
    }
    pub fn check_context(&self, context: &TurnExecutionContext) -> Result<()> {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        if let Some((started, duration)) = self.timing
            && started.elapsed() >= duration
        {
            return Err(crate::BudgetExceeded {
                resource: crate::BudgetResource::Duration,
                limit: crate::run::millis(duration),
                used: crate::run::millis(started.elapsed()),
                requested: None,
            }
            .into());
        }
        Ok(())
    }
    pub fn validate_source(&self, source: &RunSession) -> Result<()> {
        ensure!(
            self.source.id == source.metadata().id
                && self.source.cwd == source.metadata().cwd
                && self.source.revision == source.metadata().revision
                && self.source.leaf_id == source.metadata().leaf_id,
            "stale or foreign fork source"
        );
        Ok(())
    }
    pub fn prepare_full(
        source: &Session,
        point: ForkPoint,
        limits: SessionForkLimits,
        context: &TurnExecutionContext,
    ) -> Result<Self> {
        let limits = limits.validate()?;
        ensure!(
            source.entries().len() <= limits.max_source_entries,
            "fork source entry budget exceeded"
        );
        crate::extensions::size(&source.entries(), limits.max_source_bytes as usize)?;
        let (point, leaf, editor) = resolve_fork_point(source, &point, limits.max_editor_bytes)?;
        let branch = source.branch(leaf.as_deref())?;
        let mut records = Vec::new();
        let mut bytes = 0usize;
        for entry in branch.entries() {
            if context.is_cancelled() {
                return Err(context.error().into());
            }
            bytes = bytes.saturating_add(crate::extensions::size(*entry, limits.max_bytes)?);
            ensure!(
                records.len() < limits.max_entries && bytes <= limits.max_bytes,
                "fork prefix budget exceeded"
            );
            records.push((*entry).clone());
        }
        Self::from_records(
            source.metadata().clone(),
            point,
            leaf,
            editor,
            records,
            source.labels().clone(),
            0,
            limits,
            context,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn from_records(
        source: SessionMetadata,
        point: ForkPoint,
        copied_leaf: Option<String>,
        editor: Option<ForkEditor>,
        mut records: Vec<SessionEntry>,
        labels: BTreeMap<String, String>,
        ignored_tail: u64,
        limits: SessionForkLimits,
        context: &TurnExecutionContext,
    ) -> Result<Self> {
        let limits = limits.validate()?;
        ensure!(
            records.len() <= limits.max_entries,
            "fork entry budget exceeded"
        );
        crate::extensions::size(&records, limits.max_bytes)?;
        let mut previous = None;
        let ids = records
            .iter()
            .map(|e| e.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        for entry in &records {
            ensure!(
                entry.parent_id == previous,
                "fork records must form the complete selected ancestry"
            );
            previous = Some(entry.id.clone());
        }
        ensure!(previous == copied_leaf, "fork prefix leaf mismatch");
        let ids = ids
            .into_iter()
            .map(str::to_owned)
            .collect::<std::collections::HashSet<_>>();
        let mut archived = 0;
        for entry in &mut records {
            if context.is_cancelled() {
                return Err(context.error().into());
            }
            if let SessionEntryData::Extension { namespace, data } = &entry.data
                && namespace == NAVIGATION_NAMESPACE
            {
                let nav: NavigationRecord = serde_json::from_value(data.clone())?;
                if matches!(&nav,NavigationRecord::Label{entry_id,..}if !ids.contains(entry_id)) {
                    entry.data = SessionEntryData::Extension {
                        namespace: "aicoder.session.fork.omitted-navigation.v1".into(),
                        data: serde_json::json!({"reason":"target_outside_copied_ancestry","original":data}),
                    };
                    archived += 1;
                }
            }
        }
        let now = super::now_millis()?;
        let mut metadata = SessionMetadata {
            version: SESSION_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            cwd: source.cwd.clone(),
            created_at: now,
            updated_at: now,
            title: None,
            leaf_id: copied_leaf.clone(),
            revision: 1,
        };
        let mut snapshot = Session::from_entries(metadata.clone(), records)?;
        let provenance = ForkProvenance {
            source_session_id: source.id.clone(),
            source_revision: source.revision,
            source_selected_leaf: source.leaf_id.clone(),
            copied_leaf,
            point: point.clone(),
            copied_entries: snapshot.entries().len(),
            navigation_records_archived: archived,
            source_ignored_tail_bytes: ignored_tail,
        };
        let mut extra = Vec::new();
        if let Some(turn) = snapshot.unfinished_turn() {
            extra.push(SessionEntryData::TurnEnd{turn_id:turn.into(),record:Box::new(TurnRecord{pending_inputs:Vec::new(),stop:Some(crate::RunStop::Cancelled{reason:"Fork boundary retained a partial turn; its later completion is outside this copied prefix".into()}),partial_response:None,statistics:None})});
        }
        if let Some(title) = &source.title {
            extra.push(
                NavigationRecord::Rename {
                    title: title.clone(),
                }
                .data(),
            );
        }
        for id in snapshot.labels().keys() {
            if !labels.contains_key(id) {
                extra.push(
                    NavigationRecord::Label {
                        entry_id: id.clone(),
                        label: None,
                    }
                    .data(),
                );
            }
        }
        for (id, label) in labels {
            if ids.contains(&id) {
                extra.push(
                    NavigationRecord::Label {
                        entry_id: id,
                        label: Some(label),
                    }
                    .data(),
                );
            }
        }
        extra.push(SessionEntryData::Extension {
            namespace: FORK_NAMESPACE.into(),
            data: serde_json::to_value(&provenance)?,
        });
        let transaction = snapshot.transaction(extra)?;
        let mut all = snapshot.entries().to_vec();
        all.extend(transaction.entries);
        metadata.leaf_id = transaction.leaf_id;
        ensure!(
            all.len() <= limits.max_entries,
            "fork derived metadata exceeds entry budget"
        );
        crate::extensions::size(&all, limits.max_bytes)?;
        snapshot = Session::from_entries(metadata, all)?;
        snapshot.selected_branch().model_context()?;
        Ok(Self {
            source,
            point,
            target: RunSession::Full(Box::new(snapshot)),
            editor,
            provenance,
            limits,
            backend: None,
            window_preview: None,
            timing: None,
        })
    }
}
/// Resolve the user's explicit point without performing IO or moving the source selection.
pub fn resolve_fork_point(
    source: &Session,
    point: &ForkPoint,
    max_editor_bytes: usize,
) -> Result<(ForkPoint, Option<String>, Option<ForkEditor>)> {
    match point {
        ForkPoint::BeforeLastUser => {
            let id = source
                .selected_branch()
                .entries()
                .iter()
                .rev()
                .find(|e| {
                    e.data
                        .as_message()
                        .is_some_and(|m| m.role == MessageRole::User)
                })
                .map(|e| e.id.clone())
                .ok_or_else(|| anyhow::anyhow!("fork requires a recorded user message"))?;
            resolve_fork_point(source, &ForkPoint::Before(id), max_editor_bytes)
        }
        ForkPoint::Current => Ok((ForkPoint::Current, source.metadata().leaf_id.clone(), None)),
        ForkPoint::Root => Ok((ForkPoint::Root, None, None)),
        ForkPoint::At(value) => {
            let id = source.resolve_entry(value)?.to_owned();
            Ok((ForkPoint::At(id.clone()), Some(id), None))
        }
        ForkPoint::Before(value) => {
            let id = source.resolve_entry(value)?.to_owned();
            let entry = source.entry(&id).expect("resolved fork entry");
            let message = entry
                .data
                .as_message()
                .filter(|m| m.role == MessageRole::User)
                .ok_or_else(|| anyhow::anyhow!("fork-before requires a canonical user message"))?;
            let editor = fork_editor(&id, message, max_editor_bytes);
            Ok((ForkPoint::Before(id), entry.parent_id.clone(), Some(editor)))
        }
    }
}
pub fn fork_editor(id: &str, message: &Message, max_editor_bytes: usize) -> ForkEditor {
    let text = message.text_content();
    let mut end = text.len().min(max_editor_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    ForkEditor {
        source_entry_id: id.into(),
        text: text[..end].into(),
        text_complete: end == text.len(),
        image_blocks_omitted: message
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::Image { .. }))
            .count(),
    }
}
pub struct PublishedSessionFork {
    pub session: RunSession,
    pub lease: SessionRunLease,
}
#[derive(Debug, Clone)]
pub struct SessionForkPublishFailure {
    pub visible: Option<SessionMetadata>,
    pub stage: &'static str,
}
impl std::fmt::Display for SessionForkPublishFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(m) = &self.visible {
            write!(
                f,
                "fork target {} became visible, but {} failed; inspect it before retrying",
                m.id, self.stage
            )
        } else {
            write!(f, "session fork {} failed before publication", self.stage)
        }
    }
}
impl std::error::Error for SessionForkPublishFailure {}

/// Source ownership spans model preflight, guards, publication and completion observations.
pub struct PreparedSessionFork<'a> {
    repository: &'a dyn SessionRepository,
    source: RunSession,
    plan: SessionForkPlan,
    _lease: SessionRunLease,
}
impl<'a> PreparedSessionFork<'a> {
    pub fn prepare(
        repository: &'a dyn SessionRepository,
        workspace: &std::path::Path,
        id: &str,
        point: ForkPoint,
        limits: SessionForkLimits,
        context: &TurnExecutionContext,
    ) -> Result<Self> {
        let nav = SessionNavigator::new(repository, workspace)?;
        let mut source = nav.open_run(id)?;
        let lease = source.acquire(repository)?;
        let plan = repository.prepare_fork(&source, point, limits, context)?;
        plan.validate_source(&source)?;
        Ok(Self {
            repository,
            source,
            plan,
            _lease: lease,
        })
    }
    pub fn plan(&self) -> &SessionForkPlan {
        &self.plan
    }
    pub(crate) fn publish(
        self,
        context: &TurnExecutionContext,
    ) -> Result<(PublishedSessionFork, SessionRunLease)> {
        let published = self
            .repository
            .publish_fork(&self.source, self.plan, context)?;
        Ok((published, self._lease))
    }
}
pub struct SessionForkOutcome {
    pub session: RunSession,
    pub editor: Option<ForkEditor>,
    pub provenance: ForkProvenance,
    pub reports: Vec<SessionActionReport>,
}
impl crate::Agent {
    pub async fn fork_session(
        &self,
        repository: &dyn SessionRepository,
        id: &str,
        point: ForkPoint,
        limits: SessionForkLimits,
        context: TurnExecutionContext,
    ) -> Result<SessionForkOutcome> {
        let context = context
            .child()
            .with_preparation_start(tokio::time::Instant::now());
        let prepared = PreparedSessionFork::prepare(
            repository,
            self.turn_executor().workspace_root(),
            id,
            point,
            limits,
            &context,
        )?;
        self.apply_fork(prepared, context).await
    }
    pub async fn apply_fork(
        &self,
        mut prepared: PreparedSessionFork<'_>,
        context: TurnExecutionContext,
    ) -> Result<SessionForkOutcome> {
        use crate::extensions::*;
        let context = context
            .child()
            .with_preparation_start(tokio::time::Instant::now());
        if let Some(duration) = self.turn_executor().execution_config().budget.max_duration {
            prepared.plan.timing = Some((
                context.preparation_start(tokio::time::Instant::now()),
                duration,
            ));
        }
        let plan = prepared.plan();
        ensure!(
            plan.source().cwd == self.turn_executor().workspace_root(),
            "fork source belongs to another workspace"
        );
        let source = SessionPosition::from(plan.source());
        let target = SessionPosition::from(plan.target());
        let operation = SessionOperation::Fork {
            source_session_id: source.id.clone(),
            point: plan.point().clone(),
        };
        let preview = plan.preview();
        let input = self.prepare_session_view(&preview, &context).await?;
        self.session_before(&operation, Some(&source), Some(&target), &context)
            .await?;
        self.turn_executor().validate_model_selection(&input)?;
        self.turn_executor().check_preparation_budget(&input)?;
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let editor = plan.editor().cloned();
        let provenance = plan.provenance().clone();
        let expected = plan.target().clone();
        let (published, source_lease) = prepared.publish(&context)?;
        let mut session = published.session;
        if session.metadata().id != expected.id
            || session.metadata().revision != expected.revision
            || session.metadata().leaf_id != expected.leaf_id
        {
            return Err(SessionForkPublishFailure {
                visible: Some(session.metadata().clone()),
                stage: "backend acknowledgement identity",
            }
            .into());
        }
        if let Some(authority) = &context.approval_sessions {
            authority.remove(self.turn_executor().workspace_root(), &source.id);
            authority.remove(self.turn_executor().workspace_root(), &expected.id);
        }
        let mut reports = Vec::new();
        for kind in [SessionEventKind::Shutdown, SessionEventKind::Started] {
            reports.push(
                self.session_after(
                    SessionLifecycleEvent {
                        kind,
                        reason: Some(SessionLifecycleReason::Fork),
                        operation: operation.clone(),
                        source: Some(source.clone()),
                        target: Some(SessionPosition::from(session.metadata())),
                    },
                    &context,
                )
                .await,
            );
        }
        drop(published.lease);
        drop(source_lease);
        session.clear_run_token();
        Ok(SessionForkOutcome {
            session,
            editor,
            provenance,
            reports,
        })
    }
}
