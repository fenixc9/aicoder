//! Offline host adoption and navigation, with full or cold-window JSONL storage.
use aicoder_core::{extensions::*, model::*, session::*, *};
use aicoder_runtime::session::{JsonlSessionRepository, SessionLoadLimits};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
struct Provider;
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("offline", m);
        s.context_tokens = Some(65536);
        Ok(s)
    }
    async fn generate(
        &self,
        _: ModelRequest,
        _: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        panic!("no model IO in session lifecycle example")
    }
}
struct Policy {
    deny: bool,
    fail: bool,
    events: Mutex<Vec<SessionLifecycleEvent>>,
}
#[async_trait]
impl SessionHook for Policy {
    async fn before(
        &self,
        i: SessionHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<SessionDecision> {
        Ok(
            if self.deny
                && matches!(
                    i.operation,
                    SessionOperation::Navigation {
                        action: ArchiveNavigation::Rename { .. }
                    }
                )
            {
                SessionDecision::Deny {
                    reason: "host policy".into(),
                }
            } else {
                SessionDecision::Continue
            },
        )
    }
    async fn after(&self, e: &SessionLifecycleEvent, _: ExtensionCancellation) -> Result<()> {
        self.events.lock().unwrap().push(e.clone());
        anyhow::ensure!(!self.fail, "PRIVATE_OBSERVATION_ERROR");
        Ok(())
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.iter()
            .all(|a| matches!(a.as_str(), "--window" | "--deny" | "--observer-error")),
        "use --window, --deny and/or --observer-error"
    );
    let window = args.iter().any(|s| s == "--window");
    let deny = args.iter().any(|s| s == "--deny");
    let fail = args.iter().any(|s| s == "--observer-error");
    let root = tempfile::tempdir()?;
    let store = tempfile::tempdir()?;
    let full = JsonlSessionRepository::new(store.path())?;
    let hook = Arc::new(Policy {
        deny,
        fail,
        events: Mutex::new(Vec::new()),
    });
    let mut e = Extension::new("session-fixture", "1")?;
    e.session_hook(HookSpec::new("policy"), hook.clone())?;
    let mut registry = ExtensionRegistry::new();
    registry.install(e)?;
    let a = Agent::new(
        TurnExecutor::builder(Provider)
            .workspace(root.path())
            .extensions(Arc::new(registry.finish()))
            .build()?,
        AgentConfig::new("offline"),
    );
    let selected = a
        .select_session(
            &full,
            None,
            SessionSelection::New,
            TurnExecutionContext::new(),
        )
        .await?;
    let id = selected.session.metadata().id.clone();
    let mut session = full.open(&id)?;
    for i in 0..12 {
        full.append(
            &mut session,
            Message::text(
                if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                format!("record-{i}-{}", "x".repeat(256)),
            ),
        )?;
    }
    let path = session.path().unwrap().to_owned();
    let before = std::fs::read(&path)?;
    let repo = JsonlSessionRepository::with_limits(
        store.path(),
        SessionLoadLimits {
            max_snapshot_bytes: if window { 1024 } else { 16 * 1024 * 1024 },
            max_window_messages: 2,
            ..Default::default()
        },
    )?;
    let nav = SessionNavigator::new(&repo, root.path())?;
    let prepared = nav.prepare(
        &id,
        ArchiveNavigation::Branch {
            source: BranchSource::Entry(session.entries()[1].id.clone()),
            name: Some("frozen".into()),
        },
    )?;
    let leaf = prepared.plan().target().leaf_id.clone();
    ensure!(std::fs::read(&path)? == before, "preparation wrote bytes");
    let state = a
        .apply_navigation(prepared, TurnExecutionContext::new())
        .await?
        .session;
    ensure!(
        state.metadata().leaf_id == leaf,
        "commit changed its prepared identity"
    );
    ensure!(
        matches!(state, RunSession::Window(_)) == window,
        "unexpected storage view"
    );
    let source = std::fs::read(&path)?;
    let result = a
        .navigate_session(
            &repo,
            &id,
            ArchiveNavigation::Rename {
                title: "renamed".into(),
            },
            TurnExecutionContext::new(),
        )
        .await;
    let incomplete = if deny {
        ensure!(
            result.is_err() && std::fs::read(&path)? == source,
            "denial changed history"
        );
        0
    } else {
        let result = result?;
        ensure!(
            result.session.metadata().title.as_deref() == Some("renamed"),
            "adoption missing"
        );
        ensure!(
            std::fs::read(&path)?.starts_with(&source),
            "old branch bytes changed"
        );
        result.report.delivery.failures.len()
    };
    let shutdown = a
        .notify_session_shutdown(
            &repo,
            &id,
            SessionShutdownReason::Quit,
            TurnExecutionContext::new(),
        )
        .await?;
    ensure!(
        shutdown.delivery.completed == usize::from(!fail),
        "shutdown receipt inaccurate"
    );
    println!(
        "{}",
        serde_json::json!({"window":window,"denied":deny,"observer_error":fail,"read_only_preparation":true,"owned_branch_identity":true,"source_bytes_preserved":true,"incomplete_navigation_observations":incomplete,"new_user_or_tool_execution":false,"model_requests":0,"network_requests":0})
    );
    Ok(())
}
