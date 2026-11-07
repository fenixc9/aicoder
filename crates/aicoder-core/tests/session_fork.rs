use aicoder_core::{extensions::*, model::*, session::*, summary::*, *};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
struct Provider;
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", m);
        s.context_tokens = Some(65536);
        s.capabilities.tools = true;
        Ok(s)
    }
    async fn generate(
        &self,
        _: ModelRequest,
        _: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        panic!("fork must not invoke models")
    }
}
#[derive(Default)]
struct Hook {
    deny: bool,
    events: Mutex<Vec<SessionLifecycleEvent>>,
}
#[async_trait]
impl SessionHook for Hook {
    async fn before(
        &self,
        _: SessionHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<SessionDecision> {
        Ok(if self.deny {
            SessionDecision::Deny {
                reason: "host policy".into(),
            }
        } else {
            SessionDecision::Continue
        })
    }
    async fn after(&self, e: &SessionLifecycleEvent, _: ExtensionCancellation) -> Result<()> {
        self.events.lock().unwrap().push(e.clone());
        Ok(())
    }
}
fn agent(root: &std::path::Path, h: Arc<dyn SessionHook>) -> Agent {
    let mut e = Extension::new("fork", "1").unwrap();
    e.session_hook(HookSpec::new("guard"), h).unwrap();
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
                message: Message::text(MessageRole::User, "first"),
            },
            SessionEntryData::Message {
                message: Message::text(MessageRole::Assistant, "answer"),
            },
            SessionEntryData::Message {
                message: Message::text(MessageRole::User, "edit this user"),
            },
            SessionEntryData::Message {
                message: Message::text(MessageRole::Assistant, "later answer"),
            },
        ],
    )
    .unwrap();
    s
}
#[tokio::test]
async fn clone_and_before_at_forks_keep_source_and_owned_record_ids_and_do_not_execute_models() {
    for point_kind in 0..3 {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let s = seed(&repo, root.path());
        let h = Arc::new(Hook::default());
        let a = agent(root.path(), h.clone());
        let point = match point_kind {
            0 => ForkPoint::Current,
            1 => ForkPoint::Before(s.entries()[2].id.clone()),
            _ => ForkPoint::At(s.entries()[2].id.clone()),
        };
        let prepared = PreparedSessionFork::prepare(
            &repo,
            root.path(),
            &s.metadata().id,
            point,
            SessionForkLimits::default(),
            &TurnExecutionContext::new(),
        )
        .unwrap();
        let planned = prepared.plan().target().clone();
        assert_eq!(repo.list(root.path()).unwrap().len(), 1);
        let output = a
            .apply_fork(prepared, TurnExecutionContext::new())
            .await
            .unwrap();
        assert_eq!(output.session.metadata(), &planned);
        assert_ne!(planned.id, s.metadata().id);
        assert_eq!(
            repo.open(&s.metadata().id).unwrap().metadata(),
            s.metadata()
        );
        let forked = repo.open(&planned.id).unwrap();
        let n = match point_kind {
            0 => 4,
            1 => 2,
            _ => 3,
        };
        assert_eq!(forked.messages().len(), n);
        for entry in &s.entries()[..n] {
            assert!(forked.entry(&entry.id).is_some());
        }
        assert_eq!(
            output.editor.as_ref().map(|e| e.text.as_str()),
            if point_kind == 1 {
                Some("edit this user")
            } else {
                None
            }
        );
        assert_eq!(output.reports.len(), 2);
        assert!(
            h.events
                .lock()
                .unwrap()
                .iter()
                .all(|e| matches!(e.reason, Some(SessionLifecycleReason::Fork)))
        );
        assert_eq!(output.provenance.copied_entries, n);
    }
}
#[tokio::test]
async fn deny_cancel_and_prefix_limits_leave_no_target_and_release_source_ownership() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let a = agent(
        root.path(),
        Arc::new(Hook {
            deny: true,
            ..Default::default()
        }),
    );
    assert!(
        a.fork_session(
            &repo,
            &s.metadata().id,
            ForkPoint::Current,
            SessionForkLimits::default(),
            TurnExecutionContext::new()
        )
        .await
        .is_err()
    );
    let context = TurnExecutionContext::new();
    context.cancel("cancel fork");
    assert!(
        a.fork_session(
            &repo,
            &s.metadata().id,
            ForkPoint::Current,
            SessionForkLimits::default(),
            context
        )
        .await
        .is_err()
    );
    assert!(
        PreparedSessionFork::prepare(
            &repo,
            root.path(),
            &s.metadata().id,
            ForkPoint::Current,
            SessionForkLimits {
                max_entries: 1,
                ..Default::default()
            },
            &TurnExecutionContext::new()
        )
        .is_err()
    );
    assert_eq!(repo.list(root.path()).unwrap().len(), 1);
    let mut source = repo.open(&s.metadata().id).unwrap();
    let _lease = repo.acquire_run(&mut source).unwrap();
}
#[tokio::test]
async fn labels_outside_ancestry_become_inert_and_global_labels_title_remain_correct() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let s = seed(&repo, root.path());
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let old = s.entries()[3].id.clone();
    nav.checkout(&s.metadata().id, Some(&s.entries()[1].id))
        .unwrap();
    nav.label(&s.metadata().id, &old, Some("outside")).unwrap();
    nav.label(&s.metadata().id, &s.entries()[0].id, Some("inside"))
        .unwrap();
    nav.rename(&s.metadata().id, "global title").unwrap();
    let source = repo.open(&s.metadata().id).unwrap();
    let a = agent(root.path(), Arc::new(Hook::default()));
    let fork = a
        .fork_session(
            &repo,
            &s.metadata().id,
            ForkPoint::Current,
            SessionForkLimits::default(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let target = repo.open(&fork.session.metadata().id).unwrap();
    assert_eq!(target.metadata().title.as_deref(), Some("global title"));
    assert_eq!(target.messages().len(), 2);
    assert_eq!(target.resolve_entry("inside").unwrap(), s.entries()[0].id);
    assert!(target.resolve_entry("outside").is_err());
    assert!(target.entry(&old).is_none());
    assert_eq!(fork.provenance.navigation_records_archived, 1);
    assert_eq!(
        repo.open(&source.metadata().id).unwrap().entries().len(),
        source.entries().len()
    );
}
#[tokio::test]
async fn summary_anchors_custom_context_and_partial_turn_facts_survive_without_restoring_queues() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = seed(&repo, root.path());
    let leaf = s.metadata().leaf_id.clone().unwrap();
    let cp = SummaryCheckpoint {
        version: 1,
        source_leaf: leaf,
        source_revision: s.metadata().revision,
        first_kept_entry_id: s.entries()[2].id.clone(),
        summarized_messages: 2,
        accounted_by_turn: None,
        draft: SummaryDraft {
            summary: ConversationSummary {
                goals: vec!["preserved summary".into()],
                ..Default::default()
            },
            keep_messages: 2,
            source_digest: "0123456789abcdef".into(),
            provider: "fixture".into(),
            model: "m".into(),
            usage: ModelUsage::zero(),
            tokens: Default::default(),
            attempts: 1,
            uncertain_operations: false,
            archived_raw_messages: 0,
        },
    };
    repo.append_entries(&mut s, vec![cp.data()]).unwrap();
    repo.append_entries(
        &mut s,
        vec![
            SessionEntryData::CustomMessage {
                custom: CustomMessage::text("sender", "note", "custom")
                    .unwrap()
                    .with_display(false),
            },
            SessionEntryData::native_context(
                NativeContextSource {
                    extension: "fixture".into(),
                    hook: "source".into(),
                    kind: "context".into(),
                },
                "real context",
            )
            .unwrap(),
            SessionEntryData::TurnStart {
                turn_id: "partial".into(),
            },
            SessionEntryData::Message {
                message: Message::text(MessageRole::User, "unfinished user"),
            },
        ],
    )
    .unwrap();
    let a = agent(root.path(), Arc::new(Hook::default()));
    let copied = a
        .fork_session(
            &repo,
            &s.metadata().id,
            ForkPoint::Current,
            SessionForkLimits::default(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let target = repo.open(&copied.session.metadata().id).unwrap();
    assert!(target.unfinished_turn().is_none());
    assert!(
        repo.open(&s.metadata().id)
            .unwrap()
            .unfinished_turn()
            .is_some()
    );
    let view = target
        .selected_branch()
        .model_context()
        .unwrap()
        .into_messages();
    assert!(
        view.iter()
            .any(|m| m.text_content().contains("preserved summary"))
    );
    assert!(target.entries().iter().any(|e|matches!(&e.data,SessionEntryData::NativeContext{message,..}if message.text_content().contains("real context"))));
    assert!(target.entries().iter().any(|e|matches!(&e.data,SessionEntryData::TurnEnd{record,..}if record.statistics.is_none()&&record.stop.is_some())));
    assert!(a.custom_inbox().pending(&target.metadata().id).is_empty());
}
#[tokio::test]
async fn source_authority_retirement_follows_publication_and_empty_root_forks_keep_source() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let s = seed(repo.as_ref(), root.path());
    let cache = aicoder_core::tools::ApprovalSessions::default();
    let scope = cache.session(root.path(), &s.metadata().id).unwrap();
    let a = agent(root.path(), Arc::new(Hook::default()));
    let output = a
        .fork_session(
            repo.as_ref(),
            &s.metadata().id,
            ForkPoint::Root,
            SessionForkLimits::default(),
            TurnExecutionContext::new().with_approval_sessions(cache),
        )
        .await
        .unwrap();
    assert!(scope.snapshot().session.closed);
    let target = repo.open(&output.session.metadata().id).unwrap();
    assert!(target.messages().is_empty());
    assert_ne!(target.metadata().id, s.metadata().id);
    let mut source = repo.open(&s.metadata().id).unwrap();
    let _lease = repo.acquire_run(&mut source).unwrap();
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
async fn cancelled_post_publication_observation_keeps_both_owned_sessions_and_reports_the_new_identity()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(MemorySessionRepository::new());
    let source = seed(repo.as_ref(), root.path());
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
    let id = source.metadata().id.clone();
    let task = tokio::spawn(async move {
        a.fork_session(
            repository.as_ref(),
            &id,
            ForkPoint::Current,
            SessionForkLimits::default(),
            owned,
        )
        .await
    });
    started.notified().await;
    let sessions = repo.list(root.path()).unwrap();
    assert_eq!(sessions.len(), 2);
    for session in sessions {
        let mut s = repo.open(&session.id).unwrap();
        assert!(repo.acquire_run(&mut s).is_err());
    }
    context.cancel("cancel fork observer");
    let result = task.await.unwrap().unwrap();
    assert_ne!(result.session.metadata().id, source.metadata().id);
    assert!(
        result
            .reports
            .iter()
            .any(|r| !r.delivery.failures.is_empty())
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), ended.notified())
        .await
        .unwrap();
    let mut target = repo.open(&result.session.metadata().id).unwrap();
    let _lease = repo.acquire_run(&mut target).unwrap();
}
#[tokio::test]
async fn latest_user_resolution_is_selected_branch_local_and_editor_clipping_is_explicit() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let source = seed(&repo, root.path());
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    nav.checkout(&source.metadata().id, Some(&source.entries()[1].id))
        .unwrap();
    let a = agent(root.path(), Arc::new(Hook::default()));
    let result = a
        .fork_session(
            &repo,
            &source.metadata().id,
            ForkPoint::BeforeLastUser,
            SessionForkLimits {
                max_editor_bytes: 3,
                ..Default::default()
            },
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    let editor = result.editor.unwrap();
    assert_eq!(editor.source_entry_id, source.entries()[0].id);
    assert_eq!(editor.text, "fir");
    assert!(!editor.text_complete);
    assert!(
        repo.open(&result.session.metadata().id)
            .unwrap()
            .messages()
            .is_empty()
    );
}
