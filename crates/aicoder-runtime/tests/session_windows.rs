use aicoder_core::{model::*, session::*, *};
use aicoder_runtime::session::{JsonlSessionRepository, SessionLoadLimits};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
struct Model(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        Ok(ModelSpec::new("fixture", model))
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let model = request.model.clone();
        self.0.lock().unwrap().push(request);
        let response = ModelResponse {
            id: None,
            model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::test]
async fn large_archive_runs_from_a_real_canonical_tail_and_appends_without_loading_a_snapshot() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(storage.path()).unwrap();
    let mut session = writer.create(workspace.path()).unwrap();
    for i in 0..12 {
        writer
            .append(
                &mut session,
                Message::text(MessageRole::User, format!("old-{i} {}", "x".repeat(1000))),
            )
            .unwrap();
    }
    writer
        .append(&mut session, Message::text(MessageRole::User, "latest"))
        .unwrap();
    let original = std::fs::read(session.path().unwrap()).unwrap();
    let reader = JsonlSessionRepository::with_limits(
        storage.path(),
        SessionLoadLimits {
            max_snapshot_bytes: 1024,
            max_window_bytes: 3000,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(reader.open(&session.metadata().id).is_err());
    let state = reader.open_run(&session.metadata().id).unwrap();
    let RunSession::Window(window) = state else {
        panic!("expected bounded window")
    };
    assert!(window.source().archived_messages > 0);
    assert!(window.messages().len() < session.messages().len());
    assert!(
        window
            .entries()
            .iter()
            .filter_map(|entry| match &entry.data {
                SessionEntryData::Message { message } => Some(message.text_content()),
                _ => None,
            })
            .all(|text| !text.contains("private index"))
    );
    assert!(
        window.model_context().unwrap()[0]
            .text_content()
            .contains("Archive window omitted")
    );
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        TurnExecutor::builder(Model(requests.clone()))
            .workspace(workspace.path())
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let outcome = agent
        .run_with_session_outcome(
            &reader,
            SessionSelection::Existing(session.metadata().id.clone()),
            "continue",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(outcome, AgentTurnOutcome::Completed(_)),
        "{outcome:?}"
    );
    assert!(
        outcome
            .progress()
            .model_context
            .as_ref()
            .unwrap()
            .source
            .as_ref()
            .unwrap()
            .archived_messages
            > 0
    );
    assert!(
        !requests.lock().unwrap()[0]
            .messages
            .iter()
            .any(|message| message.text_content().contains("old-0"))
    );
    assert!(
        std::fs::read(session.path().unwrap())
            .unwrap()
            .starts_with(&original)
    );
    let full = writer.open(&session.metadata().id).unwrap();
    assert_eq!(full.messages().len(), session.messages().len() + 2);
}

fn limited(root: &std::path::Path) -> JsonlSessionRepository {
    JsonlSessionRepository::with_limits(
        root,
        SessionLoadLimits {
            max_snapshot_bytes: 512,
            max_window_bytes: 4000,
            ..Default::default()
        },
    )
    .unwrap()
}
fn seed(repo: &JsonlSessionRepository, workspace: &std::path::Path) -> Session {
    let mut session = repo.create(workspace).unwrap();
    for i in 0..10 {
        repo.append(
            &mut session,
            Message::text(
                MessageRole::User,
                format!("needle-{i} {}", "x".repeat(1000)),
            ),
        )
        .unwrap();
    }
    repo.append(&mut session, Message::text(MessageRole::User, "latest"))
        .unwrap();
    session
}
#[test]
fn paged_records_search_tree_and_navigation_use_real_payloads_and_reject_stale_cursors() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(storage.path()).unwrap();
    let session = seed(&full, workspace.path());
    let repo = limited(storage.path());
    let nav = SessionNavigator::new(&repo, workspace.path()).unwrap();
    let id = &session.metadata().id;
    let first = nav.archive(id, 2, None).unwrap();
    assert_eq!(first.entries.len(), 2);
    assert_eq!(
        serde_json::to_value(&first.entries).unwrap(),
        serde_json::to_value(&session.entries()[..2]).unwrap()
    );
    let cursor = first.next.unwrap();
    assert_eq!(
        nav.archive(id, 2, Some(&cursor)).unwrap().entries[0].id,
        session.entries()[2].id
    );
    assert!(
        nav.tree(id, 2, None).unwrap().rows[0]
            .preview
            .contains("needle-0")
    );
    assert_eq!(nav.search(id, "needle-0", 10, None).unwrap().hits.len(), 1);
    nav.execute(
        id,
        ArchiveNavigation::Rename {
            title: "global".into(),
        },
    )
    .unwrap();
    assert!(nav.archive(id, 2, Some(&cursor)).is_err());
    nav.execute(
        id,
        ArchiveNavigation::Label {
            entry_id: session.entries()[0].id.clone(),
            label: Some("root-message".into()),
        },
    )
    .unwrap();
    let fork = nav
        .execute(
            id,
            ArchiveNavigation::Branch {
                source: BranchSource::Entry("root-message".into()),
                name: Some("branch".into()),
            },
        )
        .unwrap();
    assert_eq!(fork.messages().len(), 1);
    assert_eq!(nav.info(id).unwrap().title.as_deref(), Some("global"));
    assert!(full.open(id).unwrap().resolve_entry("branch").is_ok());
}
#[test]
fn window_conflicts_same_length_edits_and_foreign_repositories_cannot_write() {
    use std::io::{Seek, SeekFrom, Write};
    let storage = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(storage.path()).unwrap();
    let mut session = seed(&full, workspace.path());
    let repo = limited(storage.path());
    let RunSession::Window(mut stale) = repo.open_run(&session.metadata().id).unwrap() else {
        panic!()
    };
    full.append(&mut session, Message::text(MessageRole::User, "new"))
        .unwrap();
    assert!(
        repo.append_window(
            &mut stale,
            vec![SessionEntryData::Message {
                message: Message::text(MessageRole::User, "stale")
            }]
        )
        .is_err()
    );
    let RunSession::Window(mut window) = repo.open_run(&session.metadata().id).unwrap() else {
        panic!()
    };
    let path = session.path().unwrap();
    std::fs::copy(path, other.path().join(path.file_name().unwrap())).unwrap();
    assert!(
        limited(other.path())
            .append_window(&mut window, Vec::new())
            .is_err()
    );
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let bytes = std::fs::read(path).unwrap();
    let at = bytes
        .windows(8)
        .position(|part| part == b"needle-0")
        .unwrap();
    file.seek(SeekFrom::Start(at as u64)).unwrap();
    file.write_all(b"NEEDLE-0").unwrap();
    drop(file);
    assert!(repo.append_window(&mut window, Vec::new()).is_err());
    assert!(repo.open_run(&session.metadata().id).is_err());
}
#[test]
fn record_index_scan_and_page_budgets_fail_explicitly_without_mutation() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(storage.path()).unwrap();
    let session = seed(&full, workspace.path());
    let before = std::fs::read(session.path().unwrap()).unwrap();
    for limits in [
        SessionLoadLimits {
            max_snapshot_bytes: 512,
            max_record_bytes: 512,
            ..Default::default()
        },
        SessionLoadLimits {
            max_snapshot_bytes: 512,
            max_index_entries: 2,
            ..Default::default()
        },
        SessionLoadLimits {
            max_snapshot_bytes: 512,
            max_scan_bytes: 1024,
            ..Default::default()
        },
    ] {
        let repo = JsonlSessionRepository::with_limits(storage.path(), limits).unwrap();
        assert!(repo.open_run(&session.metadata().id).is_err());
    }
    let repo = JsonlSessionRepository::with_limits(
        storage.path(),
        SessionLoadLimits {
            max_snapshot_bytes: 512,
            max_page_bytes: 128,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(repo.archive_page(&session.metadata().id, 1, None).is_err());
    assert_eq!(std::fs::read(session.path().unwrap()).unwrap(), before);
}
#[test]
fn newest_oversized_tool_unit_is_not_split_and_torn_tail_is_repaired_as_one_batch() {
    use serde_json::json;
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(storage.path()).unwrap();
    let mut session = seed(&full, workspace.path());
    full.append_entries(
        &mut session,
        vec![
            SessionEntryData::Message {
                message: Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall {
                        call: ModelToolCall {
                            id: "call".into(),
                            name: "work".into(),
                            arguments: json!({}),
                        },
                    }],
                },
            },
            SessionEntryData::Message {
                message: Message {
                    role: MessageRole::Tool,
                    content: vec![ContentBlock::ToolResult {
                        call_id: "call".into(),
                        content: json!({"large":"x".repeat(6000)}),
                        is_error: false,
                    }],
                },
            },
        ],
    )
    .unwrap();
    assert!(
        limited(storage.path())
            .open_run(&session.metadata().id)
            .is_err()
    );
    full.append(
        &mut session,
        Message::text(MessageRole::User, "after tools"),
    )
    .unwrap();
    let path = session.path().unwrap();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b"{\"type\":\"transaction\",BROKEN")
        .unwrap();
    let repo = limited(storage.path());
    let RunSession::Window(mut window) = repo.open_run(&session.metadata().id).unwrap() else {
        panic!()
    };
    repo.append_window(
        &mut window,
        vec![SessionEntryData::Message {
            message: Message::text(MessageRole::User, "repaired"),
        }],
    )
    .unwrap();
    assert!(!std::fs::read_to_string(path).unwrap().contains("BROKEN"));
    assert_eq!(
        full.open(&session.metadata().id)
            .unwrap()
            .messages()
            .last()
            .unwrap()
            .message
            .text_content(),
        "repaired"
    );
    assert!(window.model_context().is_err());
}
#[test]
fn window_leases_and_workspace_scope_are_enforced_without_restoring_authority() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(storage.path()).unwrap();
    let session = seed(&full, workspace.path());
    let repo = limited(storage.path());
    let RunSession::Window(mut owner) = repo.open_run(&session.metadata().id).unwrap() else {
        panic!()
    };
    let lease = repo.acquire_window(&mut owner).unwrap();
    let RunSession::Window(mut other) = repo.open_run(&session.metadata().id).unwrap() else {
        panic!()
    };
    assert!(other.run_token().is_none());
    assert!(repo.append_window(&mut other, Vec::new()).is_err());
    assert!(
        SessionNavigator::new(&repo, foreign.path())
            .unwrap()
            .open_run(&session.metadata().id)
            .is_err()
    );
    drop(lease);
    assert!(repo.append_window(&mut other, Vec::new()).is_ok());
}

#[tokio::test]
async fn durable_summary_and_canonical_tail_survive_window_loading_and_new_checkpoints() {
    use aicoder_core::{context::*, summary::*};
    use serde_json::json;
    struct Summaries;
    #[async_trait]
    impl ModelProvider for Summaries {
        fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
            let mut spec = ModelSpec::new("fixture", model);
            spec.context_tokens = Some(16384);
            spec.max_output_tokens = Some(128);
            Ok(spec)
        }
        async fn generate(
            &self,
            request: ModelRequest,
            sink: Arc<dyn ModelEventSink>,
            _: TurnExecutionContext,
        ) -> Result<ModelResponse, ProviderError> {
            let text = if request.messages[0]
                .text_content()
                .starts_with("Summarize historical")
            {
                json!({"goals":["finish"],"constraints":["KEEP_CONSTRAINT"],"changes":[],"todos":["test"],"notes":[]}).to_string()
            } else {
                assert!(
                    request
                        .messages
                        .iter()
                        .any(|m| m.text_content().contains("KEEP_CONSTRAINT"))
                );
                "done".into()
            };
            let response = ModelResponse {
                id: None,
                model: request.model,
                message: Message::text(MessageRole::Assistant, text),
                stop_reason: StopReason::Complete,
                usage: ModelUsage::zero(),
            };
            emit_response(&response, sink.as_ref());
            Ok(response)
        }
    }
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let full = JsonlSessionRepository::new(storage.path()).unwrap();
    let session = seed(&full, workspace.path());
    let provider = Arc::new(Summaries);
    let mut config = SummaryConfig::new(ContextWindowConfig {
        max_context_tokens: 1200,
        reserved_output_tokens: 200,
        preserve_recent_tokens: 80,
    });
    config.output_tokens = 128;
    config.max_input_tokens = 1400;
    let agent = Agent::new(
        TurnExecutor::builder_from_shared(provider.clone())
            .workspace(workspace.path())
            .context_compactor(SemanticContextCompactor::new(provider, "fixture", config).unwrap())
            .build()
            .unwrap(),
        AgentConfig::new("fixture"),
    );
    let report = agent
        .compact_session(&full, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(report.succeeded(), "{report:?}");
    let repo = limited(storage.path());
    let state = repo.open_run(&session.metadata().id).unwrap();
    let (_, context) = state.source_and_context().unwrap();
    assert!(
        context
            .iter()
            .any(|m| m.text_content().contains("KEEP_CONSTRAINT"))
    );
    assert!(
        !context
            .iter()
            .any(|m| m.text_content().contains("private index"))
    );
    let result = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "continue",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(
        matches!(result, AgentTurnOutcome::Completed(_)),
        "{result:?}"
    );
    assert_eq!(
        full.open(&session.metadata().id).unwrap().messages().len(),
        session.messages().len() + 2
    );
    let mut growing = full.open(&session.metadata().id).unwrap();
    for _ in 0..2 {
        full.append(
            &mut growing,
            Message::text(MessageRole::User, "more context ".repeat(80)),
        )
        .unwrap();
    }
    let report = agent
        .compact_session(&repo, &session.metadata().id, TurnExecutionContext::new())
        .await;
    assert!(report.succeeded(), "{report:?}");
    let saved = full.open(&session.metadata().id).unwrap();
    let SessionEntryData::Extension { data, .. } = &saved.entries().last().unwrap().data else {
        panic!()
    };
    let checkpoint: SummaryCheckpoint = serde_json::from_value(data.clone()).unwrap();
    assert!(checkpoint.draft.archived_raw_messages > 0);
}
