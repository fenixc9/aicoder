use aicoder_core::{extensions::*, model::*, session::*, summary::*, *};
use aicoder_runtime::session::{JsonlSessionRepository, SessionLoadLimits};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Provider(Mutex<Vec<ModelRequest>>);
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", m);
        s.context_tokens = Some(65536);
        Ok(s)
    }
    async fn generate(
        &self,
        r: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let n = {
            let mut rs = self.0.lock().unwrap();
            rs.push(r.clone());
            rs.len()
        };
        let response = ModelResponse {
            id: None,
            model: r.model,
            message: Message::text(MessageRole::Assistant, format!("answer-{n}")),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Drafts {
    again: bool,
    extra: bool,
    cold_edit: Option<(String, ContextReplacement)>,
    cold_keep: Option<String>,
}
#[async_trait]
impl BoundaryHook for Drafts {
    async fn boundary(
        &self,
        i: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        if i.kind != BoundaryKind::TurnEnd || i.round != 1 {
            return Ok(None);
        }
        let custom = |text: &str| BoundaryEntry::CustomMessage {
            kind: "note".into(),
            content: vec![ContentBlock::Text { text: text.into() }],
            display: true,
            details: Some(json!({"inert":true})),
        };
        let mut append = vec![];
        if let Some((id, replacement)) = &self.cold_edit {
            append.push(BoundaryEntry::ContextEdit {
                target_id: id.clone(),
                replacement: replacement.clone(),
            });
        }
        if self.extra {
            append.push(custom("before-compaction"));
        }
        append.push(BoundaryEntry::Compaction {
            summary: "native summary".into(),
            first_kept_entry_id: self.cold_keep.clone(),
            details: Some(json!({"instructions":"PRIVATE_INERT_DETAILS"})),
        });
        if self.extra {
            append.push(custom("after-compaction"));
        }
        Ok(Some(BoundaryAction {
            append,
            continue_run: Some(self.again),
            ..Default::default()
        }))
    }
}
fn agent(root: &std::path::Path, p: Arc<Provider>, hook: Option<Drafts>) -> Agent {
    let mut builder = TurnExecutor::builder_from_shared(p).workspace(root);
    if let Some(hook) = hook {
        let mut e = Extension::new("drafts", "1").unwrap();
        e.boundary_hook(HookSpec::new("stage"), Arc::new(hook))
            .unwrap();
        let mut r = ExtensionRegistry::new();
        r.install(e).unwrap();
        builder = builder.extensions(Arc::new(r.finish()));
    }
    Agent::new(
        builder.build().unwrap(),
        AgentConfig::new("fixture").system_prompt("fixed runtime prefix"),
    )
}
fn seed(repo: &JsonlSessionRepository, root: &std::path::Path) -> Session {
    let mut s = repo.create(root).unwrap();
    for i in 0..12 {
        repo.append(
            &mut s,
            Message::text(MessageRole::User, format!("raw-{i} {}", "x".repeat(600))),
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
            max_window_messages: 2,
            ..Default::default()
        },
    )
    .unwrap()
}
fn native(s: &Session) -> NativeCompactionRecord {
    s.entries()
        .iter()
        .rev()
        .find_map(|e| match &e.data {
            SessionEntryData::Extension { namespace, data }
                if namespace == NATIVE_COMPACTION_NAMESPACE =>
            {
                Some(serde_json::from_value(data.clone()).unwrap())
            }
            _ => None,
        })
        .unwrap()
}
#[tokio::test]
async fn live_window_clears_old_projection_but_keeps_following_custom_and_future_messages() {
    let root = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(store.path()).unwrap();
    let s = seed(&writer, root.path());
    let before = std::fs::read(s.path().unwrap()).unwrap();
    let repo = limited(store.path());
    let p = Arc::new(Provider::default());
    let a = agent(
        root.path(),
        p.clone(),
        Some(Drafts {
            again: true,
            extra: true,
            cold_edit: None,
            cold_keep: None,
        }),
    );
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    assert!(
        std::fs::read(s.path().unwrap())
            .unwrap()
            .starts_with(&before)
    );
    let full = writer.open(&s.metadata().id).unwrap();
    let n = native(&full);
    assert_eq!(n.replaced_messages, 15);
    assert_eq!(n.retained_messages, 0);
    assert!(n.uncertain_operations);
    assert_eq!(full.messages().len(), 17);
    let rs = p.0.lock().unwrap();
    assert_eq!(rs.len(), 2);
    let text = serde_json::to_string(&rs[1]).unwrap();
    assert!(text.contains("after-compaction"));
    assert!(
        !text.contains("before-compaction")
            && !text.contains("raw-")
            && !text.contains("PRIVATE_INERT")
    );
    let state = limited(store.path()).open_run(&s.metadata().id).unwrap();
    let (source, context) = state.source_and_context().unwrap();
    assert_eq!(source.recorded_messages, 17);
    assert_eq!(context.last().unwrap().text_content(), "answer-2");
    assert!(
        context
            .iter()
            .any(|m| m.text_content().contains("after-compaction"))
    );
    assert!(
        context
            .iter()
            .all(|m| !m.text_content().contains("private index"))
    );
}
#[tokio::test]
async fn zero_message_window_after_self_retaining_compaction_can_resume_normally() {
    let root = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(store.path()).unwrap();
    let s = seed(&writer, root.path());
    let repo = limited(store.path());
    let p = Arc::new(Provider::default());
    let a = agent(
        root.path(),
        p.clone(),
        Some(Drafts {
            again: false,
            extra: false,
            cold_edit: None,
            cold_keep: None,
        }),
    );
    assert!(matches!(
        a.run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            Arc::new(()),
            TurnExecutionContext::new()
        )
        .await,
        AgentTurnOutcome::Completed(_)
    ));
    let state = limited(store.path()).open_run(&s.metadata().id).unwrap();
    assert!(state.messages().is_empty());
    assert_eq!(state.source_and_context().unwrap().0.archived_messages, 14);
    let a = agent(root.path(), p.clone(), None);
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "future input",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    let saved = writer.open(&s.metadata().id).unwrap();
    assert_eq!(saved.messages().len(), 16);
    assert!(
        saved
            .selected_branch()
            .model_context()
            .unwrap()
            .messages
            .iter()
            .any(|m| m.text_content() == "future input")
    );
    assert_eq!(
        saved
            .selected_branch()
            .model_context()
            .unwrap()
            .messages
            .last()
            .unwrap()
            .text_content(),
        "answer-2"
    );
}
#[tokio::test]
async fn cold_edit_is_validated_against_real_source_without_projecting_private_descriptors() {
    let root = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(store.path()).unwrap();
    let mut s = writer.create(root.path()).unwrap();
    let call = ContentBlock::ToolCall {
        call: ModelToolCall {
            id: "old-call".into(),
            name: "read".into(),
            arguments: json!({"path":"owned"}),
        },
    };
    writer
        .append(
            &mut s,
            Message {
                role: MessageRole::Assistant,
                content: vec![call.clone()],
            },
        )
        .unwrap();
    let id = s.metadata().leaf_id.clone().unwrap();
    writer
        .append(
            &mut s,
            Message {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    call_id: "old-call".into(),
                    content: json!({"output":"done"}),
                    is_error: false,
                }],
            },
        )
        .unwrap();
    for _ in 0..8 {
        writer
            .append(&mut s, Message::text(MessageRole::User, "tail".repeat(200)))
            .unwrap();
    }
    let repo = limited(store.path());
    let p = Arc::new(Provider::default());
    let a = agent(
        root.path(),
        p,
        Some(Drafts {
            again: false,
            extra: false,
            cold_keep: None,
            cold_edit: Some((
                id.clone(),
                ContextReplacement::Blocks(vec![
                    call,
                    ContentBlock::Text {
                        text: "cold edited rationale".into(),
                    },
                ]),
            )),
        }),
    );
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(matches!(out, AgentTurnOutcome::Completed(_)), "{out:?}");
    let full = writer.open(&s.metadata().id).unwrap();
    assert_eq!(
        full.entry(&id)
            .unwrap()
            .data
            .as_message()
            .unwrap()
            .tool_calls()
            .next()
            .unwrap()
            .arguments["path"],
        "owned"
    );
    assert!(full.entries().iter().any(|e|matches!(&e.data,SessionEntryData::Extension {namespace,..} if namespace==CONTEXT_EDIT_NAMESPACE)));
    assert!(
        !full
            .selected_branch()
            .model_context()
            .unwrap()
            .messages
            .iter()
            .any(|m| m.text_content().contains("cold edited rationale"))
    );
}
#[tokio::test]
async fn unadmitted_cold_tail_and_prepared_source_conflicts_never_publish_a_partial_reduction() {
    let root = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(store.path()).unwrap();
    let s = seed(&writer, root.path());
    let cold = s.entries()[0].id.clone();
    let repo = limited(store.path());
    let p = Arc::new(Provider::default());
    let a = agent(
        root.path(),
        p.clone(),
        Some(Drafts {
            again: true,
            extra: false,
            cold_edit: None,
            cold_keep: Some(cold),
        }),
    );
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "task",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    assert!(!matches!(out, AgentTurnOutcome::Completed(_)));
    assert_eq!(p.0.lock().unwrap().len(), 1);
    let full = writer.open(&s.metadata().id).unwrap();
    assert!(!full.entries().iter().any(|e|matches!(&e.data,SessionEntryData::Extension {namespace,..} if namespace==NATIVE_COMPACTION_NAMESPACE)));
    let mut state = repo.open_run(&s.metadata().id).unwrap();
    let commit = BoundaryCommit {
        message_finalizations: vec![],
        startup: None,
        kind: BoundaryKind::TurnEnd,
        round: 1,
        base_index: 0,
        entries: vec![BoundaryProposal {
            entry_id: None,
            extension: "fixture".into(),
            hook: "prepare".into(),
            entry: BoundaryEntry::Compaction {
                summary: "read-only preview".into(),
                first_kept_entry_id: None,
                details: None,
            },
        }],
        continue_run: false,
        changes: vec![],
    };
    let before = std::fs::read(s.path().unwrap()).unwrap();
    let plan = state.prepare_append(&repo, &commit, None).unwrap();
    assert_eq!(std::fs::read(s.path().unwrap()).unwrap(), before);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(s.path().unwrap())
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    assert!(state.commit_append(&repo, plan).is_err());
    assert_eq!(
        std::fs::read(s.path().unwrap()).unwrap(),
        [before, b"\n".to_vec()].concat()
    );
}

#[test]
fn cold_completed_native_receipts_retain_known_usage_without_a_terminal_record() {
    let root = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let writer = JsonlSessionRepository::new(store.path()).unwrap();
    let mut s = writer.create(root.path()).unwrap();
    let spec = ModelSpec::new("native", "summary");
    let request = ModelRequest::new("summary", vec![Message::text(MessageRole::User, "source")]);
    let input_count = tokens::TokenCounting::default().request(&request, &spec);
    let attempt = auxiliary::AuxiliaryAttempt {
        id: "completed-native".into(),
        call_id: "boundary:fixture/hook".into(),
        provider: "native".into(),
        model: "summary".into(),
        status: "completed".into(),
        usage: ModelUsage {
            input_tokens: Some(2),
            output_tokens: Some(1),
            total_tokens: Some(3),
            cached_input_tokens: Some(0),
            cache_write_tokens: Some(0),
        },
        reported_usage: true,
        tokens: run::TokenAccounting {
            used: 3,
            estimated: false,
        },
        input_count,
        delivery_complete: true,
    };
    writer
        .append_entries(
            &mut s,
            vec![
                SessionEntryData::TurnStart {
                    turn_id: "unclosed".into(),
                },
                SessionEntryData::Extension {
                    namespace: auxiliary::AUXILIARY_NAMESPACE.into(),
                    data: serde_json::to_value(auxiliary::AuxiliaryRecord::Finished { attempt })
                        .unwrap(),
                },
            ],
        )
        .unwrap();
    for _ in 0..8 {
        writer
            .append(&mut s, Message::text(MessageRole::User, "tail".repeat(200)))
            .unwrap();
    }
    let repo = limited(store.path());
    let loaded = repo.open_run(&s.metadata().id).unwrap();
    assert_eq!(loaded.accounting().unwrap().usage.total_tokens, Some(3));
    assert_eq!(loaded.accounting().unwrap().tokens.used, 3);
    let RunSession::Window(w) = loaded else {
        panic!()
    };
    assert!(!w.entries().iter().any(|e|matches!(&e.data,SessionEntryData::Extension {namespace,..} if namespace==auxiliary::AUXILIARY_NAMESPACE)));
}
