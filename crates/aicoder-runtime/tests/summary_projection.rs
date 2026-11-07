use aicoder_core::{context::*, model::*, session::*, summary::*, *};
use aicoder_runtime::session::{JsonlSessionRepository, SessionLoadLimits};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Provider(Mutex<Vec<ModelRequest>>);
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("script", m);
        s.context_tokens = Some(65536);
        s.max_output_tokens = Some(128);
        Ok(s)
    }
    async fn generate(
        &self,
        r: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let summarize = r.messages[0]
            .text_content()
            .starts_with("Summarize historical");
        if summarize {
            assert!(r.tools.is_empty());
            assert_eq!(r.tool_choice, ModelToolChoice::None);
        }
        let response = ModelResponse {
            id: None,
            model: r.model.clone(),
            message: Message::text(
                MessageRole::Assistant,
                if summarize {
                    json!({"goals":["retained summary"],"constraints":[],"changes":[],"todos":[],"notes":[]}).to_string()
                } else {
                    "done".into()
                },
            ),
            stop_reason: StopReason::Complete,
            usage: ModelUsage {
                input_tokens: Some(8),
                output_tokens: Some(2),
                total_tokens: Some(10),
                ..Default::default()
            },
        };
        self.0.lock().unwrap().push(r);
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
fn agent(root: &std::path::Path, p: Arc<Provider>, recent: usize) -> Agent {
    let mut c = SummaryConfig::new(ContextWindowConfig {
        max_context_tokens: 1200,
        reserved_output_tokens: 200,
        preserve_recent_tokens: recent,
    });
    c.output_tokens = 128;
    c.max_input_tokens = 4096;
    c.max_calls = 32;
    c.failure_policy = SummaryFailurePolicy::Stop;
    Agent::new(
        TurnExecutor::builder_from_shared(p.clone())
            .workspace(root)
            .context_compactor(SemanticContextCompactor::new(p, "fixture", c).unwrap())
            .build()
            .unwrap(),
        AgentConfig::new("fixture").system_prompt("runtime prefix"),
    )
}
fn seed(repo: &JsonlSessionRepository, root: &std::path::Path) -> Session {
    let mut s = repo.create(root).unwrap();
    for i in 0..24 {
        repo.append(
            &mut s,
            Message::text(MessageRole::User, format!("raw-{i} {}", "x".repeat(1200))),
        )
        .unwrap();
    }
    s
}
fn limited(path: &std::path::Path) -> JsonlSessionRepository {
    JsonlSessionRepository::with_limits(
        path,
        SessionLoadLimits {
            max_snapshot_bytes: 512,
            max_window_bytes: 16 * 1024,
            max_window_messages: 6,
            ..Default::default()
        },
    )
    .unwrap()
}
fn edit(repo: &dyn SessionRepository, s: &Session, target: &str, replacement: ContextReplacement) {
    SessionNavigator::new(repo, &s.metadata().cwd)
        .unwrap()
        .execute(
            &s.metadata().id,
            ArchiveNavigation::EditContext {
                edit: ContextEditRecord {
                    version: 1,
                    extension: "fixture".into(),
                    hook: "edit".into(),
                    target_id: target.into(),
                    replacement,
                },
            },
        )
        .unwrap();
}
fn cp(s: &Session) -> SummaryCheckpoint {
    s.entries()
        .iter()
        .rev()
        .find_map(|e| match &e.data {
            SessionEntryData::Extension { namespace, data } if namespace == SUMMARY_NAMESPACE => {
                Some(serde_json::from_value(data.clone()).unwrap())
            }
            _ => None,
        })
        .unwrap()
}
#[tokio::test]
async fn cold_manual_summary_keeps_latest_edited_anchor_and_reopens_with_real_payloads() {
    let root = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(storage.path()).unwrap();
    let s = seed(&writer, root.path());
    let target = s.metadata().leaf_id.clone().unwrap();
    let repo = limited(storage.path());
    edit(
        &repo,
        &s,
        &target,
        ContextReplacement::Text("edited latest".into()),
    );
    let before = std::fs::read(s.path().unwrap()).unwrap();
    let provider = Arc::new(Provider::default());
    let report = agent(root.path(), provider.clone(), 1)
        .compact_session(&repo, &s.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(
        report.succeeded() && report.checkpoint_entry_id.is_some(),
        "{report:?}"
    );
    let full = writer.open(&s.metadata().id).unwrap();
    let checkpoint = cp(&full);
    assert_eq!(checkpoint.version, 2);
    assert_eq!(checkpoint.first_kept_entry_id, target);
    assert_eq!(checkpoint.draft.keep_messages, 1);
    assert_eq!(checkpoint.summarized_messages, 23);
    assert_eq!(checkpoint.draft.archived_raw_messages, 18);
    assert!(
        std::fs::read(s.path().unwrap())
            .unwrap()
            .starts_with(&before)
    );
    assert_eq!(full.messages().len(), 24);
    assert!(
        full.messages()
            .last()
            .unwrap()
            .message
            .text_content()
            .starts_with("raw-23")
    );
    let state = limited(storage.path()).open_run(&s.metadata().id).unwrap();
    let view = state.source_and_context().unwrap().1;
    assert_eq!(view.last().unwrap().text_content(), "edited latest");
    assert!(
        view.iter()
            .all(|m| !m.text_content().contains("private index"))
    );
    assert_eq!(report.usage.total_tokens, Some(report.attempts as u64 * 10));
}
#[tokio::test]
async fn cold_automatic_summary_binds_live_edited_tail_before_main_model_io() {
    let root = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(storage.path()).unwrap();
    let s = seed(&writer, root.path());
    let target = s.metadata().leaf_id.clone().unwrap();
    let repo = limited(storage.path());
    edit(
        &repo,
        &s,
        &target,
        ContextReplacement::Text("edited historical tail".into()),
    );
    let provider = Arc::new(Provider::default());
    let a = agent(root.path(), provider.clone(), 160);
    let outcome = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "fresh input",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "{outcome:?}"
    );
    let full = writer.open(&s.metadata().id).unwrap();
    let checkpoint = cp(&full);
    assert_eq!(checkpoint.first_kept_entry_id, target);
    assert_eq!(checkpoint.draft.keep_messages, 2);
    assert_eq!(checkpoint.summarized_messages, 23);
    assert!(checkpoint.accounted_by_turn.is_some());
    assert_eq!(full.messages().len(), 26);
    let requests = provider.0.lock().unwrap();
    let main = requests
        .iter()
        .find(|r| {
            !r.messages[0]
                .text_content()
                .starts_with("Summarize historical")
        })
        .unwrap();
    assert!(
        main.messages
            .iter()
            .any(|m| m.text_content() == "edited historical tail")
    );
    assert!(
        main.messages
            .iter()
            .any(|m| m.text_content() == "fresh input")
    );
    assert!(
        !main
            .messages
            .iter()
            .any(|m| m.text_content().contains("raw-18"))
    );
    assert_eq!(
        full.selected_branch()
            .accounting()
            .unwrap()
            .usage
            .total_tokens,
        Some(requests.len() as u64 * 10)
    );
}
#[tokio::test]
async fn cold_mixed_role_summary_counts_raw_custom_and_native_context_and_keeps_anchors() {
    for native in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let writer = JsonlSessionRepository::new(storage.path()).unwrap();
        let mut s = seed(&writer, root.path());
        let data = if native {
            SessionEntryData::native_context(
                NativeContextSource {
                    extension: "source".into(),
                    hook: "hook".into(),
                    kind: "context".into(),
                },
                "native tail",
            )
            .unwrap()
        } else {
            SessionEntryData::CustomMessage {
                custom: CustomMessage::text("source", "kind", "custom tail").unwrap(),
            }
        };
        writer.append_entries(&mut s, vec![data]).unwrap();
        let target = s.metadata().leaf_id.clone().unwrap();
        let repo = limited(storage.path());
        let report = agent(root.path(), Arc::new(Provider::default()), 1)
            .compact_session(&repo, &s.metadata().id, TurnExecutionContext::new())
            .await;
        assert!(
            report.succeeded() && report.checkpoint_entry_id.is_some(),
            "{report:?}"
        );
        let full = writer.open(&s.metadata().id).unwrap();
        let checkpoint = cp(&full);
        assert_eq!(checkpoint.first_kept_entry_id, target);
        assert_eq!(checkpoint.summarized_messages, 24);
        assert_eq!(checkpoint.draft.keep_messages, 1);
        let reopened = limited(storage.path()).open_run(&s.metadata().id).unwrap();
        let (_, view) = reopened.source_and_context().unwrap();
        assert_eq!(
            view.last().unwrap(),
            full.messages().last().unwrap().message
        );
    }
}
#[test]
fn live_window_projection_uses_only_admitted_records_and_rejects_stale_or_foreign_turns() {
    let root = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(storage.path()).unwrap();
    let s = seed(&writer, root.path());
    let repo = limited(storage.path());
    let mut state = repo.open_run(&s.metadata().id).unwrap();
    let old = match &state {
        RunSession::Window(w) => w.clone(),
        _ => panic!(),
    };
    let lease = state.acquire(&repo).unwrap();
    state
        .append(
            &repo,
            vec![
                SessionEntryData::TurnStart {
                    turn_id: "owned-turn".into(),
                },
                SessionEntryData::Message {
                    message: Message::text(MessageRole::User, "live input"),
                },
            ],
        )
        .unwrap();
    let RunSession::Window(w) = &state else {
        panic!()
    };
    assert!(w.model_context().is_err());
    assert!(repo.project_window_context(&old, None).is_err());
    assert!(state.project_context(&repo, Some("foreign")).is_err());
    let before = std::fs::read(s.path().unwrap()).unwrap();
    let projection = state
        .project_context(&repo, Some("owned-turn"))
        .unwrap()
        .unwrap();
    assert_eq!(projection.source.interrupted_turns, 0);
    assert_eq!(
        projection.messages.last().unwrap().text_content(),
        "live input"
    );
    assert!(
        projection
            .entry_ids
            .iter()
            .flatten()
            .all(|id| w.entries().iter().any(|e| &e.id == id))
    );
    assert_eq!(std::fs::read(s.path().unwrap()).unwrap(), before);
    assert_eq!(
        state
            .project_context(&repo, None)
            .unwrap()
            .unwrap()
            .source
            .interrupted_turns,
        1
    );
    drop(lease);
}
