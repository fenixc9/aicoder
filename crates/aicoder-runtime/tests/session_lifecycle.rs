use aicoder_core::{extensions::*, model::*, session::*, *};
use aicoder_runtime::session::{JsonlSessionRepository, SessionLoadLimits};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
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
        panic!("offline metadata only")
    }
}
struct Hook {
    deny: bool,
    seen: Mutex<Vec<SessionOperation>>,
}
#[async_trait]
impl SessionHook for Hook {
    async fn before(
        &self,
        i: SessionHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<SessionDecision> {
        self.seen.lock().unwrap().push(i.operation.clone());
        Ok(if self.deny {
            SessionDecision::Deny {
                reason: "denied".into(),
            }
        } else {
            SessionDecision::Continue
        })
    }
}
fn agent(root: &std::path::Path, hook: Arc<Hook>) -> Agent {
    let mut e = Extension::new("window-policy", "1").unwrap();
    e.session_hook(HookSpec::new("guard"), hook).unwrap();
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
async fn cold_window_plan_is_read_only_and_commits_the_same_pinned_branch_without_fabricated_history()
 {
    let store = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(store.path()).unwrap();
    let mut s = full.create(root.path()).unwrap();
    for i in 0..12 {
        full.append(
            &mut s,
            Message::text(
                if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                format!("record-{i}-{}", "x".repeat(512)),
            ),
        )
        .unwrap();
    }
    let id = s.metadata().id.clone();
    let target = s.entries()[1].id.clone();
    let path = s.path().unwrap().to_owned();
    let before = std::fs::read(&path).unwrap();
    let repo = JsonlSessionRepository::with_limits(
        store.path(),
        SessionLoadLimits {
            max_snapshot_bytes: 1024,
            max_window_messages: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let plan = nav
        .prepare(
            &id,
            ArchiveNavigation::Branch {
                source: BranchSource::Entry(target.clone()),
                name: Some("cold".into()),
            },
        )
        .unwrap();
    let planned = plan.plan().target().leaf_id.clone();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let committed = plan.commit().unwrap();
    assert!(matches!(committed, RunSession::Window(_)));
    assert_eq!(committed.metadata().leaf_id, planned);
    assert!(std::fs::read(&path).unwrap().starts_with(&before));
    let hook = Arc::new(Hook {
        deny: false,
        seen: Mutex::new(Vec::new()),
    });
    let a = agent(root.path(), hook);
    let output = a
        .navigate_session(
            &repo,
            &id,
            ArchiveNavigation::Checkout {
                target: Some(target),
            },
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        output.session.metadata().leaf_id,
        Some(s.entries()[1].id.clone())
    );
    let state = full.open(&id).unwrap();
    assert_eq!(state.messages().len(), 2);
    if let RunSession::Window(w) = output.session {
        assert!(w.entries().iter().all(|e| s.entry(&e.id).is_some()));
    }
}
#[tokio::test]
async fn denied_window_changes_preserve_all_file_bytes_and_release_the_repository_lease() {
    let store = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(store.path()).unwrap();
    let mut s = full.create(root.path()).unwrap();
    for i in 0..8 {
        full.append(
            &mut s,
            Message::text(MessageRole::User, format!("record-{i}-{}", "x".repeat(512))),
        )
        .unwrap();
    }
    let path = s.path().unwrap().to_owned();
    let before = std::fs::read(&path).unwrap();
    let repo = JsonlSessionRepository::with_limits(
        store.path(),
        SessionLoadLimits {
            max_snapshot_bytes: 512,
            max_window_messages: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let h = Arc::new(Hook {
        deny: true,
        seen: Mutex::new(Vec::new()),
    });
    let a = agent(root.path(), h);
    assert!(
        a.navigate_session(
            &repo,
            &s.metadata().id,
            ArchiveNavigation::Checkout { target: None },
            TurnExecutionContext::new()
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let mut window = repo.open_run(&s.metadata().id).unwrap();
    let _lease = window.acquire(&repo).unwrap();
}
struct EditStorage(std::path::PathBuf);
#[async_trait]
impl SessionHook for EditStorage {
    async fn before(
        &self,
        _: SessionHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<SessionDecision> {
        let bytes = std::fs::read(&self.0)?;
        let mut text = String::from_utf8(bytes)?;
        text = text.replacen("record-", "Record-", 1);
        std::fs::write(&self.0, text)?;
        Ok(SessionDecision::Continue)
    }
}
#[tokio::test]
async fn storage_change_during_a_guard_never_refreshes_or_retargets_the_prepared_commit() {
    let store = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(store.path()).unwrap();
    let mut s = full.create(root.path()).unwrap();
    for i in 0..8 {
        full.append(
            &mut s,
            Message::text(MessageRole::User, format!("record-{i}-{}", "x".repeat(512))),
        )
        .unwrap();
    }
    let path = s.path().unwrap().to_owned();
    let original = std::fs::read_to_string(&path).unwrap();
    let expected = original.replacen("record-", "Record-", 1);
    let repo = JsonlSessionRepository::with_limits(
        store.path(),
        SessionLoadLimits {
            max_snapshot_bytes: 512,
            max_window_messages: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let mut e = Extension::new("external-writer", "1").unwrap();
    e.session_hook(HookSpec::new("edit"), Arc::new(EditStorage(path.clone())))
        .unwrap();
    let mut registry = ExtensionRegistry::new();
    registry.install(e).unwrap();
    let a = Agent::new(
        TurnExecutor::builder(Provider)
            .workspace(root.path())
            .extensions(Arc::new(registry.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    );
    assert!(
        a.navigate_session(
            &repo,
            &s.metadata().id,
            ArchiveNavigation::Checkout { target: None },
            TurnExecutionContext::new()
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
}
