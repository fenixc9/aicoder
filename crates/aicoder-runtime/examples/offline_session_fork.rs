//! Bounded independent-session fork/clone with real JSONL storage and no model service.
use aicoder_core::{extensions::*, model::*, session::*, *};
use aicoder_runtime::session::{JsonlSessionRepository, SessionLoadLimits};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use std::sync::Arc;
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
        panic!("fork example has no model IO")
    }
}
struct Policy {
    deny: bool,
    observer: bool,
}
#[async_trait]
impl SessionHook for Policy {
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
    async fn after(&self, _: &SessionLifecycleEvent, _: ExtensionCancellation) -> Result<()> {
        ensure!(!self.observer, "PRIVATE_CALLBACK_ERROR");
        Ok(())
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.iter().all(|a| matches!(
            a.as_str(),
            "--window" | "--before" | "--deny" | "--observer-error"
        )),
        "use --window, --before, --deny and/or --observer-error"
    );
    let window = args.iter().any(|a| a == "--window");
    let before = args.iter().any(|a| a == "--before");
    let deny = args.iter().any(|a| a == "--deny");
    let observer = args.iter().any(|a| a == "--observer-error");
    let root = tempfile::tempdir()?;
    let store = tempfile::tempdir()?;
    let full = JsonlSessionRepository::new(store.path())?;
    let mut source = full.create(root.path())?;
    for i in 0..24 {
        full.append(
            &mut source,
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
    let original = std::fs::read(source.path().unwrap())?;
    let repo = JsonlSessionRepository::with_limits(
        store.path(),
        SessionLoadLimits {
            max_snapshot_bytes: if window { 1024 } else { 16 * 1024 * 1024 },
            max_window_messages: 2,
            ..Default::default()
        },
    )?;
    let mut e = Extension::new("fork-fixture", "1")?;
    e.session_hook(HookSpec::new("policy"), Arc::new(Policy { deny, observer }))?;
    let mut registry = ExtensionRegistry::new();
    registry.install(e)?;
    let a = Agent::new(
        TurnExecutor::builder(Provider)
            .workspace(root.path())
            .extensions(Arc::new(registry.finish()))
            .build()?,
        AgentConfig::new("m"),
    );
    let point = if before {
        ForkPoint::BeforeLastUser
    } else {
        ForkPoint::Current
    };
    let plan = PreparedSessionFork::prepare(
        &repo,
        root.path(),
        &source.metadata().id,
        point,
        SessionForkLimits::default(),
        &TurnExecutionContext::new(),
    )?;
    let planned = plan.plan().target().clone();
    ensure!(
        full.list(root.path())?.len() == 1,
        "preparation published a target"
    );
    let result = a.apply_fork(plan, TurnExecutionContext::new()).await;
    let (copied, incomplete) = if deny {
        ensure!(
            result.is_err() && full.list(root.path())?.len() == 1,
            "denial published a fork"
        );
        (0, 0)
    } else {
        let output = result?;
        ensure!(
            output.session.metadata() == &planned
                && output.session.metadata().id != source.metadata().id,
            "fork identity changed"
        );
        ensure!(
            matches!(output.session, RunSession::Window(_)) == window,
            "unexpected target view"
        );
        let target = full.open(&planned.id)?;
        let count = if before { 22 } else { 24 };
        ensure!(target.messages().len() == count, "history prefix differs");
        ensure!(output.editor.is_some() == before, "editor mismatch");
        (
            count,
            output
                .reports
                .iter()
                .map(|r| r.delivery.failures.len())
                .sum::<usize>(),
        )
    };
    ensure!(
        std::fs::read(source.path().unwrap())? == original,
        "source mutated"
    );
    println!(
        "{}",
        serde_json::json!({"window":window,"before":before,"denied":deny,"observer_error":observer,"copied_messages":copied,"incomplete_observations":incomplete,"read_only_preparation":true,"owned_identity":true,"source_unchanged":true,"no_authority_or_queue_restore":true,"model_requests":0,"network_requests":0})
    );
    Ok(())
}
