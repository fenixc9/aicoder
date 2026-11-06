use aicoder_core::{extensions::*, model::*, session::*, *};
use anyhow::Result;
use async_trait::async_trait;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Default)]
struct Provider;
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", m);
        s.capabilities.tools = true;
        s.context_tokens = Some(65536);
        Ok(s)
    }
    async fn generate(
        &self,
        _: ModelRequest,
        _: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        panic!("session operations must not invoke models")
    }
}
#[derive(Default)]
struct Hook {
    deny: bool,
    fail_after: bool,
    before: Mutex<Vec<SessionOperation>>,
    after: Mutex<Vec<SessionLifecycleEvent>>,
}
#[async_trait]
impl SessionHook for Hook {
    async fn before(
        &self,
        input: SessionHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<SessionDecision> {
        self.before.lock().unwrap().push(input.operation.clone());
        Ok(if self.deny {
            SessionDecision::Deny {
                reason: "host policy".into(),
            }
        } else {
            SessionDecision::Continue
        })
    }
    async fn after(&self, event: &SessionLifecycleEvent, _: ExtensionCancellation) -> Result<()> {
        self.after.lock().unwrap().push(event.clone());
        anyhow::ensure!(!self.fail_after, "PRIVATE_OBSERVER_FAILURE");
        Ok(())
    }
}
fn agent(root: &std::path::Path, h: Arc<dyn SessionHook>) -> Agent {
    let mut e = Extension::new("session", "1").unwrap();
    e.session_hook(HookSpec::new("policy"), h).unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    Agent::new(
        TurnExecutor::builder(Provider)
            .workspace(root)
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    )
}
fn seed(repo: &dyn SessionRepository, root: &std::path::Path) -> Session {
    let mut s = repo.create(root).unwrap();
    repo.append_entries(
        &mut s,
        vec![
            SessionEntryData::Message {
                message: Message::text(MessageRole::User, "user"),
            },
            SessionEntryData::Message {
                message: Message::text(MessageRole::Assistant, "answer"),
            },
        ],
    )
    .unwrap();
    s
}
#[tokio::test]
async fn guards_run_on_resolved_owned_branch_plans_and_report_actual_adoption() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let id = s.metadata().id.clone();
    let target = s.entries()[0].id.clone();
    let h = Arc::new(Hook::default());
    let a = agent(root.path(), h.clone());
    let output = a
        .navigate_session(
            &repo,
            &id,
            ArchiveNavigation::Branch {
                source: BranchSource::Entry(target.clone()),
                name: Some("experiment".into()),
            },
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(h.before.lock().unwrap().len(), 1);
    assert_eq!(h.after.lock().unwrap().len(), 1);
    assert_eq!(output.report.delivery.completed, 1);
    assert!(
        matches!(&h.before.lock().unwrap()[0],SessionOperation::Navigation{action:ArchiveNavigation::Branch{source:BranchSource::Entry(value),..}}if value==&target)
    );
    assert_eq!(
        output.report.event.target.as_ref().unwrap().leaf_id,
        output.session.metadata().leaf_id
    );
    assert_eq!(repo.open(&id).unwrap().messages().len(), 1);
    assert_eq!(repo.open(&id).unwrap().entries().len(), 3);
}
#[tokio::test]
async fn denials_do_not_write_or_notify_after_and_release_ownership() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let h = Arc::new(Hook {
        deny: true,
        ..Default::default()
    });
    let a = agent(root.path(), h.clone());
    let error = a
        .navigate_session(
            &repo,
            &s.metadata().id,
            ArchiveNavigation::Rename {
                title: "denied".into(),
            },
            TurnExecutionContext::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(error.downcast_ref::<SessionActionDenied>().is_some());
    assert_eq!(
        repo.open(&s.metadata().id).unwrap().metadata().revision,
        s.metadata().revision
    );
    assert!(h.after.lock().unwrap().is_empty());
    let mut opened = repo.open(&s.metadata().id).unwrap();
    let lease = repo.acquire_run(&mut opened).unwrap();
    drop(lease);
}
#[tokio::test]
async fn observer_failure_preserves_completed_mutation_and_reports_bounded_safe_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let a = agent(
        root.path(),
        Arc::new(Hook {
            fail_after: true,
            ..Default::default()
        }),
    );
    let output = a
        .navigate_session(
            &repo,
            &s.metadata().id,
            ArchiveNavigation::Rename {
                title: "actual".into(),
            },
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(output.session.metadata().title.as_deref(), Some("actual"));
    assert_eq!(
        repo.open(&s.metadata().id)
            .unwrap()
            .metadata()
            .title
            .as_deref(),
        Some("actual")
    );
    assert_eq!(output.report.delivery.invoked, 1);
    assert_eq!(output.report.delivery.completed, 0);
    assert_eq!(output.report.delivery.failures[0].class, "callback_failed");
    assert!(
        !serde_json::to_string(&output.report)
            .unwrap()
            .contains("PRIVATE_OBSERVER_FAILURE")
    );
}
struct Wait {
    started: Arc<tokio::sync::Notify>,
    ended: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl SessionHook for Wait {
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
async fn cancellation_releases_private_callback_children_and_the_same_session_lease() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let s = seed(repo.as_ref(), root.path());
    let started = Arc::new(tokio::sync::Notify::new());
    let ended = Arc::new(tokio::sync::Notify::new());
    let a = agent(
        root.path(),
        Arc::new(Wait {
            started: started.clone(),
            ended: ended.clone(),
        }),
    );
    let context = TurnExecutionContext::new();
    let owned = context.clone();
    let repository = repo.clone();
    let id = s.metadata().id.clone();
    let task = tokio::spawn(async move {
        a.navigate_session(
            repository.as_ref(),
            &id,
            ArchiveNavigation::Checkout { target: None },
            owned,
        )
        .await
    });
    started.notified().await;
    context.cancel("cancel navigation");
    assert!(task.await.unwrap().is_err());
    tokio::time::timeout(Duration::from_secs(1), ended.notified())
        .await
        .unwrap();
    let mut reopened = repo.open(&s.metadata().id).unwrap();
    assert_eq!(reopened.metadata().leaf_id, s.metadata().leaf_id);
    let _lease = repo.acquire_run(&mut reopened).unwrap();
}
#[test]
fn preparation_is_read_only_resolves_labels_once_and_commits_the_same_entry_identity() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let state = nav
        .label(&s.metadata().id, &s.entries()[0].id, Some("target"))
        .unwrap();
    let before = repo.open(&s.metadata().id).unwrap();
    let prepared = nav
        .prepare(
            &state.metadata().id,
            ArchiveNavigation::Branch {
                source: BranchSource::Entry("target".into()),
                name: None,
            },
        )
        .unwrap();
    let planned = prepared.plan().target().leaf_id.clone();
    assert_eq!(
        repo.open(&s.metadata().id).unwrap().metadata().revision,
        before.metadata().revision
    );
    let committed = prepared.commit().unwrap();
    assert_eq!(committed.metadata().leaf_id, planned);
    assert_eq!(
        repo.open(&s.metadata().id).unwrap().metadata().leaf_id,
        planned
    );
}
#[tokio::test]
async fn explicit_start_resume_new_and_shutdown_events_follow_actual_adoption_only() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let h = Arc::new(Hook::default());
    let a = agent(root.path(), h.clone());
    let first = a
        .select_session(
            &repo,
            None,
            SessionSelection::New,
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(first.reports.len(), 1);
    assert!(matches!(
        first.reports[0].event.reason,
        Some(SessionLifecycleReason::Startup)
    ));
    let first_id = first.session.metadata().id.clone();
    let second = a
        .select_session(
            &repo,
            Some(&first_id),
            SessionSelection::New,
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_ne!(second.session.metadata().id, first_id);
    assert_eq!(second.reports.len(), 2);
    assert!(matches!(
        second.reports[0].event.kind,
        SessionEventKind::Shutdown
    ));
    assert!(matches!(
        second.reports[1].event.kind,
        SessionEventKind::Started
    ));
    assert!(matches!(
        second.reports[1].event.reason,
        Some(SessionLifecycleReason::New)
    ));
    let second_id = second.session.metadata().id.clone();
    let resumed = a
        .select_session(
            &repo,
            Some(&second_id),
            SessionSelection::Existing(first_id.clone()),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(resumed.session.metadata().id, first_id);
    assert!(matches!(
        resumed.reports[1].event.reason,
        Some(SessionLifecycleReason::Resume)
    ));
    let before = h.before.lock().unwrap().len();
    let same = a
        .select_session(
            &repo,
            Some(&first_id),
            SessionSelection::Existing(first_id.clone()),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert!(same.reports.is_empty());
    assert_eq!(h.before.lock().unwrap().len(), before);
    let report = a
        .notify_session_shutdown(
            &repo,
            &first_id,
            SessionShutdownReason::Quit,
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.delivery.completed, 1);
    assert!(matches!(
        report.event.reason,
        Some(SessionLifecycleReason::Quit)
    ));
}
#[tokio::test]
async fn denied_new_or_resume_creates_no_session_and_preserves_previous_session() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let first = seed(&repo, root.path());
    let second = seed(&repo, root.path());
    let h = Arc::new(Hook {
        deny: true,
        ..Default::default()
    });
    let a = agent(root.path(), h.clone());
    let before = repo.list(root.path()).unwrap().len();
    for selection in [
        SessionSelection::New,
        SessionSelection::Existing(second.metadata().id.clone()),
    ] {
        assert!(
            a.select_session(
                &repo,
                Some(&first.metadata().id),
                selection,
                TurnExecutionContext::new()
            )
            .await
            .is_err()
        );
        assert_eq!(repo.list(root.path()).unwrap().len(), before);
        assert_eq!(
            repo.open(&first.metadata().id).unwrap().metadata(),
            first.metadata()
        );
    }
    assert!(h.after.lock().unwrap().is_empty());
}
struct AfterWait {
    started: Arc<tokio::sync::Notify>,
    ended: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl SessionHook for AfterWait {
    async fn after(&self, _: &SessionLifecycleEvent, c: ExtensionCancellation) -> Result<()> {
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
async fn cancellation_during_post_commit_observation_retains_actual_selection_and_holds_lease_until_done()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let s = seed(repo.as_ref(), root.path());
    let started = Arc::new(tokio::sync::Notify::new());
    let ended = Arc::new(tokio::sync::Notify::new());
    let a = agent(
        root.path(),
        Arc::new(AfterWait {
            started: started.clone(),
            ended: ended.clone(),
        }),
    );
    let context = TurnExecutionContext::new();
    let owned = context.clone();
    let repository = repo.clone();
    let id = s.metadata().id.clone();
    let task = tokio::spawn(async move {
        a.navigate_session(
            repository.as_ref(),
            &id,
            ArchiveNavigation::Rename {
                title: "committed".into(),
            },
            owned,
        )
        .await
    });
    started.notified().await;
    let mut visible = repo.open(&s.metadata().id).unwrap();
    assert_eq!(visible.metadata().title.as_deref(), Some("committed"));
    assert!(repo.acquire_run(&mut visible).is_err());
    context.cancel("cancel observation");
    let output = task.await.unwrap().unwrap();
    assert_eq!(
        output.session.metadata().title.as_deref(),
        Some("committed")
    );
    assert_eq!(output.report.delivery.invoked, 1);
    assert_eq!(output.report.delivery.completed, 0);
    assert_eq!(output.report.delivery.failures[0].class, "cancelled");
    tokio::time::timeout(Duration::from_secs(1), ended.notified())
        .await
        .unwrap();
    let _lease = repo.acquire_run(&mut visible).unwrap();
}
struct SlowAfter;
#[async_trait]
impl SessionHook for SlowAfter {
    async fn after(&self, _: &SessionLifecycleEvent, _: ExtensionCancellation) -> Result<()> {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }
}
#[tokio::test]
async fn hosted_adoption_reports_known_created_session_when_duration_expires_before_model_io() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut e = Extension::new("slow", "1").unwrap();
    e.session_hook(HookSpec::new("after"), Arc::new(SlowAfter))
        .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let a = Agent::new(
        TurnExecutor::builder(Provider)
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .config(TurnExecutionConfig {
                budget: RunBudget {
                    max_duration: Some(Duration::from_millis(30)),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    let hosted = a
        .run_hosted_session_outcome(
            &repo,
            None,
            SessionSelection::New,
            "user",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(hosted.outcome, AgentTurnOutcome::Limited(_)));
    assert_eq!(hosted.lifecycle.len(), 1);
    assert_eq!(hosted.lifecycle[0].delivery.completed, 0);
    let metadata = hosted
        .outcome
        .session()
        .expect("acknowledged new session retained");
    assert!(repo.open(&metadata.id).unwrap().entries().is_empty());
    assert!(hosted.outcome.progress().elapsed >= Duration::from_millis(30));
}
#[tokio::test]
async fn hosted_denial_has_no_model_turn_or_created_session_and_keeps_event_trace_valid() {
    #[derive(Default)]
    struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
    impl AgentEventHandler for Events {
        fn on_raw_event(&self, e: &AgentRawEventEnvelope) {
            self.0.lock().unwrap().push(e.clone())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let a = agent(
        root.path(),
        Arc::new(Hook {
            deny: true,
            ..Default::default()
        }),
    );
    let events = Arc::new(Events::default());
    let result = a
        .run_hosted_session_outcome(
            &repo,
            None,
            SessionSelection::New,
            "user",
            events.clone(),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(result.outcome, AgentTurnOutcome::Failed(_)));
    assert!(repo.list(root.path()).unwrap().is_empty());
    let mut validator = aicoder_core::events::EventTraceValidator::new();
    for e in events.0.lock().unwrap().iter() {
        validator.observe(e).unwrap();
    }
    validator.finish().unwrap();
}
#[tokio::test]
async fn guard_deadline_and_foreign_or_busy_sources_reject_before_mutation_or_new_callbacks() {
    let root = tempfile::tempdir().unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let other = seed(&repo, foreign.path());
    let hook = Arc::new(Hook::default());
    let a = agent(root.path(), hook.clone());
    assert!(
        a.navigate_session(
            &repo,
            &other.metadata().id,
            ArchiveNavigation::Recover,
            TurnExecutionContext::new()
        )
        .await
        .is_err()
    );
    let mut owned = repo.open(&s.metadata().id).unwrap();
    let lease = repo.acquire_run(&mut owned).unwrap();
    assert!(
        a.navigate_session(
            &repo,
            &s.metadata().id,
            ArchiveNavigation::Rename {
                title: "busy".into()
            },
            TurnExecutionContext::new()
        )
        .await
        .is_err()
    );
    assert!(hook.before.lock().unwrap().is_empty());
    drop(lease);
    let started = Arc::new(tokio::sync::Notify::new());
    let ended = Arc::new(tokio::sync::Notify::new());
    let mut e = Extension::new("timeout", "1").unwrap();
    let mut spec = HookSpec::new("guard");
    spec.timeout = Duration::from_millis(15);
    e.session_hook(
        spec,
        Arc::new(Wait {
            started,
            ended: ended.clone(),
        }),
    )
    .unwrap();
    let mut r = ExtensionRegistry::new();
    r.install(e).unwrap();
    let a = Agent::new(
        TurnExecutor::builder(Provider)
            .workspace(root.path())
            .extensions(Arc::new(r.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    assert!(
        a.navigate_session(
            &repo,
            &s.metadata().id,
            ArchiveNavigation::Rename {
                title: "late".into()
            },
            TurnExecutionContext::new()
        )
        .await
        .is_err()
    );
    assert_eq!(
        repo.open(&s.metadata().id).unwrap().metadata().title,
        s.metadata().title
    );
    tokio::time::timeout(Duration::from_secs(1), ended.notified())
        .await
        .unwrap();
}
struct ClosedAtObservation {
    scope: aicoder_core::tools::ApprovalSession,
    observed: Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait]
impl SessionHook for ClosedAtObservation {
    async fn after(&self, _: &SessionLifecycleEvent, _: ExtensionCancellation) -> Result<()> {
        self.observed.store(
            self.scope.snapshot().session.closed,
            std::sync::atomic::Ordering::SeqCst,
        );
        Ok(())
    }
}
#[tokio::test]
async fn explicitly_delegated_session_authority_is_retired_at_commit_before_observation_and_not_on_denial()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let cache = aicoder_core::tools::ApprovalSessions::default();
    let scope = cache.session(root.path(), &s.metadata().id).unwrap();
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let a = agent(
        root.path(),
        Arc::new(ClosedAtObservation {
            scope: scope.clone(),
            observed: observed.clone(),
        }),
    );
    a.navigate_session(
        &repo,
        &s.metadata().id,
        ArchiveNavigation::Rename {
            title: "committed".into(),
        },
        TurnExecutionContext::new().with_approval_sessions(cache.clone()),
    )
    .await
    .unwrap();
    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(scope.snapshot().session.closed);
    let scope = cache.session(root.path(), &s.metadata().id).unwrap();
    let a = agent(
        root.path(),
        Arc::new(Hook {
            deny: true,
            ..Default::default()
        }),
    );
    assert!(
        a.navigate_session(
            &repo,
            &s.metadata().id,
            ArchiveNavigation::Rename {
                title: "denied".into()
            },
            TurnExecutionContext::new().with_approval_sessions(cache)
        )
        .await
        .is_err()
    );
    assert!(!scope.snapshot().session.closed);
}
