use aicoder_core::{extensions::*, model::*, session::*, *};
use aicoder_runtime::session::{JsonlSessionRepository, SessionLoadLimits};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
struct Provider;
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", m);
        s.context_tokens = Some(65536);
        Ok(s)
    }
    async fn generate(
        &self,
        _: ModelRequest,
        _: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        panic!("no model request")
    }
}
struct Hook {
    deny: bool,
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
                reason: "blocked".into(),
            }
        } else {
            SessionDecision::Continue
        })
    }
}
fn agent(root: &std::path::Path, deny: bool) -> Agent {
    let mut e = Extension::new("fork", "1").unwrap();
    e.session_hook(HookSpec::new("guard"), Arc::new(Hook { deny }))
        .unwrap();
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
fn seed(repo: &JsonlSessionRepository, root: &std::path::Path) -> Session {
    let mut s = repo.create(root).unwrap();
    for i in 0..24 {
        repo.append(
            &mut s,
            Message::text(
                if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                format!("message-{i}-{}", "x".repeat(512)),
            ),
        )
        .unwrap();
    }
    s
}
#[tokio::test]
async fn cold_fork_copies_complete_prefix_atomically_preserving_raw_source_and_continuation_cursor()
{
    let root = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(store.path()).unwrap();
    let source = seed(&full, root.path());
    let before = std::fs::read(source.path().unwrap()).unwrap();
    let repo = JsonlSessionRepository::with_limits(
        store.path(),
        SessionLoadLimits {
            max_snapshot_bytes: 1024,
            max_window_messages: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let a = agent(root.path(), false);
    let prepared = PreparedSessionFork::prepare(
        &repo,
        root.path(),
        &source.metadata().id,
        ForkPoint::At(source.entries()[19].id.clone()),
        SessionForkLimits::default(),
        &TurnExecutionContext::new(),
    )
    .unwrap();
    let expected = prepared.plan().target().clone();
    assert_eq!(full.list(root.path()).unwrap().len(), 1);
    let output = a
        .apply_fork(prepared, TurnExecutionContext::new())
        .await
        .unwrap();
    assert!(matches!(output.session, RunSession::Window(_)));
    assert_eq!(output.session.metadata(), &expected);
    assert_eq!(std::fs::read(source.path().unwrap()).unwrap(), before);
    let target = full.open(&expected.id).unwrap();
    assert_eq!(target.messages().len(), 20);
    assert_eq!(full.list(root.path()).unwrap().len(), 2);
    for entry in &source.entries()[..20] {
        assert_eq!(target.entry(&entry.id).unwrap().id, entry.id);
    }
    let mut opened = repo.open_run(&expected.id).unwrap();
    let _lease = opened.acquire(&repo).unwrap();
    opened
        .append(
            &repo,
            vec![SessionEntryData::Message {
                message: Message::text(MessageRole::User, "next"),
            }],
        )
        .unwrap();
    assert_eq!(full.open(&expected.id).unwrap().messages().len(), 21);
}
#[tokio::test]
async fn denial_and_output_window_limits_leave_no_target_or_partial_stage() {
    let root = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(store.path()).unwrap();
    let source = seed(&full, root.path());
    let before = std::fs::read(source.path().unwrap()).unwrap();
    let a = agent(root.path(), true);
    assert!(
        a.fork_session(
            &full,
            &source.metadata().id,
            ForkPoint::Current,
            SessionForkLimits::default(),
            TurnExecutionContext::new()
        )
        .await
        .is_err()
    );
    let bounded = JsonlSessionRepository::with_limits(
        store.path(),
        SessionLoadLimits {
            max_snapshot_bytes: 1024,
            max_window_bytes: 64,
            max_window_messages: 2,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        agent(root.path(), false)
            .fork_session(
                &bounded,
                &source.metadata().id,
                ForkPoint::Current,
                SessionForkLimits::default(),
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(source.path().unwrap()).unwrap(), before);
    assert_eq!(full.list(root.path()).unwrap().len(), 1);
    assert!(!std::fs::read_dir(store.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".fork-")
    }));
}
struct ChangeSource(std::path::PathBuf);
#[async_trait]
impl SessionHook for ChangeSource {
    async fn before(
        &self,
        _: SessionHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<SessionDecision> {
        let text = std::fs::read_to_string(&self.0)?;
        std::fs::write(&self.0, text.replacen("message-", "Message-", 1))?;
        Ok(SessionDecision::Continue)
    }
}
struct Collision(std::path::PathBuf);
#[async_trait]
impl SessionHook for Collision {
    async fn before(
        &self,
        i: SessionHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<SessionDecision> {
        std::fs::write(
            self.0.join(format!("{}.jsonl", i.target.unwrap().id)),
            b"preexisting sentinel",
        )?;
        Ok(SessionDecision::Continue)
    }
}
fn with_hook(root: &std::path::Path, h: Arc<dyn SessionHook>) -> Agent {
    let mut e = Extension::new("adversary", "1").unwrap();
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
#[tokio::test]
async fn changed_source_and_target_collisions_never_publish_a_retargeted_or_overwritten_fork() {
    for collision in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let repo = JsonlSessionRepository::new(store.path()).unwrap();
        let source = seed(&repo, root.path());
        let before = std::fs::read(source.path().unwrap()).unwrap();
        let a = if collision {
            with_hook(root.path(), Arc::new(Collision(store.path().into())))
        } else {
            with_hook(
                root.path(),
                Arc::new(ChangeSource(source.path().unwrap().into())),
            )
        };
        let prepared = PreparedSessionFork::prepare(
            &repo,
            root.path(),
            &source.metadata().id,
            ForkPoint::Current,
            SessionForkLimits::default(),
            &TurnExecutionContext::new(),
        )
        .unwrap();
        let target = store
            .path()
            .join(format!("{}.jsonl", prepared.plan().target().id));
        assert!(
            a.apply_fork(prepared, TurnExecutionContext::new())
                .await
                .is_err()
        );
        if collision {
            assert_eq!(std::fs::read(&target).unwrap(), b"preexisting sentinel");
            assert_eq!(std::fs::read(source.path().unwrap()).unwrap(), before);
        } else {
            assert!(!target.exists());
        }
        assert!(!std::fs::read_dir(store.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".fork-")
        }));
    }
}
#[tokio::test]
async fn fork_publication_partitions_records_and_returns_a_real_bounded_target_window() {
    let root = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(store.path()).unwrap();
    let source = seed(&full, root.path());
    let repo = JsonlSessionRepository::with_limits(
        store.path(),
        SessionLoadLimits {
            max_snapshot_bytes: 1024,
            max_record_bytes: 4096,
            max_window_messages: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let output = agent(root.path(), false)
        .fork_session(
            &repo,
            &source.metadata().id,
            ForkPoint::Current,
            SessionForkLimits::default(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert!(output.session.metadata().revision > 1);
    assert!(matches!(output.session, RunSession::Window(_)));
    let target = full.open(&output.session.metadata().id).unwrap();
    assert_eq!(target.messages().len(), source.messages().len());
    for line in std::fs::read(target.path().unwrap())
        .unwrap()
        .split_inclusive(|b| *b == b'\n')
    {
        assert!(line.len() <= 4096)
    }
}
struct MakeSyncFail(std::path::PathBuf);
#[async_trait]
impl SessionHook for MakeSyncFail {
    async fn before(
        &self,
        _: SessionHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<SessionDecision> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o300))?;
        Ok(SessionDecision::Continue)
    }
}
#[tokio::test]
async fn post_visibility_failure_retains_a_complete_target_and_never_silently_republishes() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    if std::fs::metadata(store.path()).unwrap().uid() == 0 {
        return;
    }
    let repo = JsonlSessionRepository::new(store.path()).unwrap();
    let source = seed(&repo, root.path());
    let a = with_hook(root.path(), Arc::new(MakeSyncFail(store.path().into())));
    let prepared = PreparedSessionFork::prepare(
        &repo,
        root.path(),
        &source.metadata().id,
        ForkPoint::Current,
        SessionForkLimits::default(),
        &TurnExecutionContext::new(),
    )
    .unwrap();
    let target_id = prepared.plan().target().id.clone();
    let result = a.apply_fork(prepared, TurnExecutionContext::new()).await;
    std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = result
        .err()
        .expect("directory fsync must fail after visibility");
    let failure = error.downcast_ref::<SessionForkPublishFailure>().unwrap();
    assert_eq!(failure.visible.as_ref().unwrap().id, target_id);
    assert_eq!(failure.stage, "directory durability");
    let target = repo.open(&target_id).unwrap();
    assert_eq!(target.messages().len(), source.messages().len());
    assert_eq!(repo.list(root.path()).unwrap().len(), 2);
    assert!(!std::fs::read_dir(store.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".fork-")
    }));
}
