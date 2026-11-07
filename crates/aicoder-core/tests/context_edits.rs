use aicoder_core::{extensions::*, model::*, session::*, *};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
fn edit(target: &str, replacement: ContextReplacement) -> ArchiveNavigation {
    ArchiveNavigation::EditContext {
        edit: ContextEditRecord {
            version: 1,
            extension: "fixture".into(),
            hook: "edit".into(),
            target_id: target.into(),
            replacement,
        },
    }
}
fn append(repo: &dyn SessionRepository, s: &mut Session, m: Message) -> String {
    repo.append(s, m).unwrap();
    s.metadata().leaf_id.clone().unwrap()
}
fn calls(ids: &[&str]) -> Message {
    Message {
        role: MessageRole::Assistant,
        content: ids
            .iter()
            .map(|id| ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: (*id).into(),
                    name: "read".into(),
                    arguments: json!({"path":"a"}),
                },
            })
            .collect(),
    }
}
fn results(ids: &[&str]) -> Message {
    Message {
        role: MessageRole::Tool,
        content: ids
            .iter()
            .map(|id| ContentBlock::ToolResult {
                call_id: (*id).into(),
                content: json!({"output":id}),
                is_error: false,
            })
            .collect(),
    }
}
#[test]
fn edits_are_id_bound_append_only_branch_local_and_restorable() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = repo.create(root.path()).unwrap();
    let first = append(&repo, &mut s, Message::text(MessageRole::User, "duplicate"));
    let second = append(&repo, &mut s, Message::text(MessageRole::User, "duplicate"));
    let raw = s.entries().to_vec();
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let changed = nav
        .execute(
            &s.metadata().id,
            edit(&second, ContextReplacement::Text("edited".into())),
        )
        .unwrap();
    let (_, context) = changed.source_and_context().unwrap();
    assert_eq!(
        context
            .iter()
            .map(Message::text_content)
            .collect::<Vec<_>>(),
        ["duplicate", "edited"]
    );
    let stored = repo.open(&s.metadata().id).unwrap();
    assert_eq!(
        serde_json::to_value(&stored.entries()[..raw.len()]).unwrap(),
        serde_json::to_value(&raw).unwrap()
    );
    let branch = stored.selected_branch();
    let projected = branch.model_context().unwrap();
    assert!(
        matches!(projected.origins[1],ContextOrigin::Edited { entry_id,.. } if entry_id==second)
    );
    assert_eq!(projected.source.recorded_messages, 2);
    let before_edit = nav
        .execute(
            &s.metadata().id,
            ArchiveNavigation::Checkout {
                target: Some(second.clone()),
            },
        )
        .unwrap();
    assert_eq!(
        before_edit.source_and_context().unwrap().1[1].text_content(),
        "duplicate"
    );
    nav.execute(&s.metadata().id, edit(&first, ContextReplacement::Omit))
        .unwrap();
    let restored = nav
        .execute(&s.metadata().id, edit(&first, ContextReplacement::Restore))
        .unwrap();
    assert_eq!(restored.source_and_context().unwrap().1.len(), 2);
    assert_eq!(repo.open(&s.metadata().id).unwrap().messages().len(), 2);
}
#[test]
fn omission_closes_complete_tool_units_without_matching_reused_call_ids() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = repo.create(root.path()).unwrap();
    append(&repo, &mut s, Message::text(MessageRole::User, "task"));
    let a = append(&repo, &mut s, calls(&["same", "other"]));
    let r = append(&repo, &mut s, results(&["other", "same"]));
    append(
        &repo,
        &mut s,
        Message::text(MessageRole::Assistant, "separator"),
    );
    append(&repo, &mut s, calls(&["same"]));
    append(&repo, &mut s, results(&["same"]));
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let edited = nav
        .execute(&s.metadata().id, edit(&r, ContextReplacement::Omit))
        .unwrap();
    let (_, view) = edited.source_and_context().unwrap();
    assert_eq!(view.len(), 4);
    assert_eq!(view[2], calls(&["same"]));
    let stored = repo.open(&s.metadata().id).unwrap();
    assert!(stored.entry(&a).is_some());
    assert_eq!(stored.messages().len(), 6);
    let restored = nav
        .execute(&s.metadata().id, edit(&r, ContextReplacement::Restore))
        .unwrap();
    assert_eq!(restored.source_and_context().unwrap().1.len(), 7); // split multi-result record
}
#[test]
fn whole_tool_replacements_split_by_projected_identity_and_preserve_recovery() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = repo.create(root.path()).unwrap();
    append(&repo, &mut s, calls(&["a", "b"]));
    let target = append(&repo, &mut s, results(&["b", "a"]));
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let replacement = results(&["b", "a"])
        .content
        .into_iter()
        .map(|mut b| {
            if let ContentBlock::ToolResult { content, .. } = &mut b {
                *content = json!({"output":"replacement"});
            }
            b
        })
        .collect();
    let changed = nav
        .execute(
            &s.metadata().id,
            edit(&target, ContextReplacement::Blocks(replacement)),
        )
        .unwrap();
    let (_, view) = changed.source_and_context().unwrap();
    for (message, expected) in view[1..].iter().zip(["a", "b"]) {
        assert_eq!(message.content.len(), 1);
        assert!(
            matches!(&message.content[0],ContentBlock::ToolResult {call_id,content,..} if call_id==expected && content["output"]=="replacement")
        );
    }
    let mut s = repo.create(root.path()).unwrap();
    let target = append(&repo, &mut s, calls(&["missing"]));
    let changed = nav
        .execute(
            &s.metadata().id,
            edit(&target, ContextReplacement::Text("call rationale".into())),
        )
        .unwrap();
    let (_, view) = changed.source_and_context().unwrap();
    assert_eq!(view[0].tool_calls().count(), 1);
    assert!(
        matches!(&view[1].content[0],ContentBlock::ToolResult { content,is_error:true,.. } if content["error"]["code"]=="execution_interrupted")
    );
}
#[test]
fn invalid_edits_fail_before_storage_and_cannot_add_calls_images_or_system_context() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = repo.create(root.path()).unwrap();
    repo.append_entries(
        &mut s,
        vec![
            SessionEntryData::native_context(
                NativeContextSource {
                    extension: "fixture".into(),
                    hook: "source".into(),
                    kind: "context".into(),
                },
                "protected",
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let system = s.metadata().leaf_id.clone().unwrap();
    let target = append(&repo, &mut s, calls(&["a"]));
    append(&repo, &mut s, results(&["a"]));
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let before = repo.open(&s.metadata().id).unwrap().metadata().clone();
    for action in [
        edit(&system, ContextReplacement::Omit),
        edit(
            &target,
            ContextReplacement::Blocks(calls(&["different"]).content),
        ),
        edit(
            &target,
            ContextReplacement::Blocks(vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://invalid/image".into(),
                },
            }]),
        ),
        edit(
            &target,
            ContextReplacement::Text("x".repeat(MAX_CONTEXT_EDIT_BYTES)),
        ),
        edit("missing", ContextReplacement::Omit),
    ] {
        assert!(nav.execute(&before.id, action).is_err());
        assert_eq!(repo.open(&before.id).unwrap().metadata(), &before);
    }
    let custom = CustomMessage::new(
        "source",
        "kind",
        vec![ContentBlock::Text {
            text: "original".into(),
        }],
    )
    .unwrap();
    repo.append_entries(
        &mut s,
        vec![SessionEntryData::CustomMessage {
            custom: custom.clone(),
        }],
    )
    .unwrap();
    let target = s.metadata().leaf_id.clone().unwrap();
    let changed = nav
        .execute(
            &before.id,
            edit(&target, ContextReplacement::Text("rewritten".into())),
        )
        .unwrap();
    assert_eq!(
        changed
            .source_and_context()
            .unwrap()
            .1
            .last()
            .unwrap()
            .content[0],
        custom.message().content[0]
    );
}
struct Provider;
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new("fixture", m);
        s.context_tokens = Some(1024);
        Ok(s)
    }
    async fn generate(
        &self,
        _: ModelRequest,
        _: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        panic!("no model IO")
    }
}
struct Guard(bool);
#[async_trait]
impl SessionHook for Guard {
    async fn before(
        &self,
        _: SessionHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<SessionDecision> {
        Ok(if self.0 {
            SessionDecision::Continue
        } else {
            SessionDecision::Deny {
                reason: "denied".into(),
            }
        })
    }
    async fn after(&self, _: &SessionLifecycleEvent, _: ExtensionCancellation) -> Result<()> {
        anyhow::bail!("observer failed")
    }
}
fn agent(root: &std::path::Path, allow: bool) -> Agent {
    let mut ext = Extension::new("fixture", "1").unwrap();
    ext.session_hook(HookSpec::new("guard"), Arc::new(Guard(allow)))
        .unwrap();
    let mut registry = ExtensionRegistry::new();
    registry.install(ext).unwrap();
    Agent::new(
        TurnExecutor::builder(Provider)
            .workspace(root)
            .extensions(Arc::new(registry.finish()))
            .build()
            .unwrap(),
        AgentConfig::new("m"),
    )
}
#[tokio::test]
async fn agent_preflights_edits_holds_ownership_and_reports_completed_effect_despite_observer_failure()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = repo.create(root.path()).unwrap();
    let target = append(&repo, &mut s, Message::text(MessageRole::User, "original"));
    let before = s.metadata().clone();
    assert!(
        agent(root.path(), false)
            .navigate_session(
                &repo,
                &before.id,
                edit(&target, ContextReplacement::Text("changed".into())),
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert_eq!(repo.open(&before.id).unwrap().metadata(), &before);
    assert!(
        agent(root.path(), true)
            .navigate_session(
                &repo,
                &before.id,
                edit(&target, ContextReplacement::Text("too large ".repeat(4000))),
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert_eq!(repo.open(&before.id).unwrap().metadata(), &before);
    let cancelled = TurnExecutionContext::new();
    cancelled.cancel("test");
    assert!(
        agent(root.path(), true)
            .navigate_session(
                &repo,
                &before.id,
                edit(&target, ContextReplacement::Omit),
                cancelled
            )
            .await
            .is_err()
    );
    assert_eq!(repo.open(&before.id).unwrap().metadata(), &before);
    let result = agent(root.path(), true)
        .navigate_session(
            &repo,
            &before.id,
            edit(&target, ContextReplacement::Text("changed".into())),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.report.delivery.failures.len(), 1);
    assert_eq!(
        result.session.source_and_context().unwrap().1[0].text_content(),
        "changed"
    );
    assert_eq!(
        repo.open(&before.id).unwrap().messages()[0]
            .message
            .text_content(),
        "original"
    );
}
#[test]
fn summaries_retain_raw_anchors_and_do_not_resurrect_edited_away_history() {
    use aicoder_core::summary::*;
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = repo.create(root.path()).unwrap();
    let old = append(&repo, &mut s, Message::text(MessageRole::User, "old"));
    let kept = append(&repo, &mut s, Message::text(MessageRole::User, "kept"));
    let last = append(&repo, &mut s, Message::text(MessageRole::User, "last"));
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    nav.execute(&s.metadata().id, edit(&kept, ContextReplacement::Omit))
        .unwrap();
    let mut s = repo.open(&s.metadata().id).unwrap();
    let checkpoint = SummaryCheckpoint {
        version: 1,
        source_leaf: s.metadata().leaf_id.clone().unwrap(),
        source_revision: s.metadata().revision,
        first_kept_entry_id: kept.clone(),
        summarized_messages: 1,
        accounted_by_turn: None,
        draft: SummaryDraft {
            summary: ConversationSummary {
                goals: vec!["summary".into()],
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
    repo.append_entries(&mut s, vec![checkpoint.data()])
        .unwrap();
    let view = s.selected_branch().model_context().unwrap().messages;
    assert_eq!(view.len(), 2);
    assert!(view[0].text_content().contains("summary"));
    assert_eq!(view[1].text_content(), "last");
    let changed = nav
        .execute(
            &s.metadata().id,
            edit(
                &old,
                ContextReplacement::Text("must remain summarized".into()),
            ),
        )
        .unwrap();
    assert_eq!(changed.source_and_context().unwrap().1, view);
    let restored = nav
        .execute(&s.metadata().id, edit(&kept, ContextReplacement::Restore))
        .unwrap();
    assert_eq!(
        restored.source_and_context().unwrap().1[1].text_content(),
        "kept"
    );
    nav.execute(&s.metadata().id, edit(&last, ContextReplacement::Omit))
        .unwrap();
    assert_eq!(repo.open(&s.metadata().id).unwrap().messages().len(), 3);
}
#[test]
fn scalar_tool_payloads_are_editable_without_panics_and_status_is_protected() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut s = repo.create(root.path()).unwrap();
    append(&repo, &mut s, calls(&["a"]));
    let raw = Message {
        role: MessageRole::Tool,
        content: vec![ContentBlock::ToolResult {
            call_id: "a".into(),
            content: json!("scalar error"),
            is_error: true,
        }],
    };
    let target = append(&repo, &mut s, raw.clone());
    let nav = SessionNavigator::new(&repo, root.path()).unwrap();
    let changed = nav
        .execute(
            &s.metadata().id,
            edit(&target, ContextReplacement::Text("display error".into())),
        )
        .unwrap();
    assert!(
        matches!(&changed.source_and_context().unwrap().1[1].content[0],ContentBlock::ToolResult {content,is_error:true,..} if content["error"]["message"]=="display error")
    );
    assert!(
        nav.execute(
            &s.metadata().id,
            edit(&target, ContextReplacement::Blocks(results(&["a"]).content))
        )
        .is_err()
    );
    assert_eq!(
        *repo.open(&s.metadata().id).unwrap().messages()[1].message,
        raw
    );
}
