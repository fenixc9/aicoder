use aicoder_core::{model::*, session::*, *};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};
fn user(text: &str) -> Message {
    Message::text(MessageRole::User, text)
}
fn data(message: Message) -> SessionEntryData {
    SessionEntryData::Message { message }
}
fn calls(ids: &[&str]) -> Message {
    Message {
        role: MessageRole::Assistant,
        content: ids
            .iter()
            .map(|id| ContentBlock::ToolCall {
                call: ModelToolCall {
                    id: (*id).into(),
                    name: "work".into(),
                    arguments: json!({}),
                },
            })
            .collect(),
    }
}
fn tool(id: &str) -> Message {
    Message {
        role: MessageRole::Tool,
        content: vec![ContentBlock::ToolResult {
            call_id: id.into(),
            content: json!({"ok":true,"id":id}),
            is_error: false,
        }],
    }
}
struct Recorder(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl ModelProvider for Recorder {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.tools = true;
        Ok(spec)
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
async fn alternate_branch_context_can_execute_without_checkout_or_sibling_leakage() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append(&mut session, user("root")).unwrap();
    let common = session.metadata().leaf_id.clone();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::ModelChanged {
                provider: "one".into(),
                model: "a".into(),
            },
            data(calls(&["a", "b"])),
            data(tool("b")),
            data(tool("a")),
        ],
    )
    .unwrap();
    let alternate = session.metadata().leaf_id.clone();
    repo.checkout(&mut session, common.as_deref()).unwrap();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::ModelChanged {
                provider: "two".into(),
                model: "b".into(),
            },
            SessionEntryData::Compaction {
                strategy: "fixture".into(),
                summary: "INERT_SUMMARY".into(),
                preserve_from: common,
                usage: ModelUsage::zero(),
            },
            data(user("SIBLING_SECRET")),
        ],
    )
    .unwrap();
    let before = session.metadata().clone();
    let branch = session.branch(alternate.as_deref()).unwrap();
    assert_eq!(branch.selected_model(), Some(("one", "a")));
    let projection = branch.model_context().unwrap();
    assert_eq!(projection.source.reordered_tool_results, 2);
    assert_eq!(projection.source.recovered_tool_results, 0);
    assert_eq!(projection.messages[2].tool_result().unwrap().0, "a");
    assert_eq!(projection.messages[3].tool_result().unwrap().0, "b");
    assert_eq!(projection.origins.len(), projection.messages.len());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let exec = TurnExecutor::builder(Recorder(requests.clone()))
        .workspace(root.path())
        .build()
        .unwrap();
    let input = ExecutionInput::from_branch(
        ModelRequest::new("offline", vec![user("continue alternate")]),
        &branch,
    )
    .unwrap();
    let result = exec.run_input(input).await.unwrap();
    let requests = requests.lock().unwrap();
    assert!(
        !requests[0]
            .messages
            .iter()
            .any(|m| m.text_content().contains("SIBLING_SECRET"))
    );
    assert!(
        !requests[0]
            .messages
            .iter()
            .any(|m| m.text_content().contains("INERT_SUMMARY"))
    );
    assert_eq!(session.metadata(), &before);
    assert_eq!(repo.open(&before.id).unwrap().metadata(), &before);
    assert_eq!(
        result
            .model_context
            .as_ref()
            .unwrap()
            .source
            .as_ref()
            .unwrap()
            .leaf_id,
        alternate
    );
    assert!(session.branch(Some("missing")).is_err());
    assert!(
        session
            .branch(None)
            .unwrap()
            .model_context()
            .unwrap()
            .messages
            .is_empty()
    );
}
#[tokio::test]
async fn raw_result_history_retains_arrival_order_and_model_history_has_recovered_results_only() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::TurnStart {
                turn_id: "crashed".into(),
            },
            data(user("old")),
            data(calls(&["a", "b"])),
            data(tool("b")),
        ],
    )
    .unwrap();
    let id = session.metadata().id.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        TurnExecutor::builder(Recorder(requests.clone()))
            .workspace(root.path())
            .build()
            .unwrap(),
        AgentConfig::new("offline").system_prompt("trusted"),
    );
    let (handle, control) = RunHandle::pair();
    handle.steer(user("redirect")).unwrap();
    let outcome = agent
        .run_with_session_controlled(
            &repo,
            SessionSelection::Existing(id.clone()),
            "new",
            Arc::new(()),
            control,
        )
        .await;
    let result = outcome.into_result().unwrap().execution_result;
    let recorded = repo.open(&id).unwrap();
    assert_eq!(recorded.chat_messages().len(), 6);
    assert!(
        !recorded
            .chat_messages()
            .iter()
            .any(|message| message.tool_result().is_some_and(|(id, _, _)| id == "a"))
    );
    assert!(!result.messages.iter().any(|message| {
        message
            .text_content()
            .contains("Previous execution was interrupted")
    }));
    assert!(
        !result
            .messages
            .iter()
            .any(|message| message.tool_result().is_some_and(|(id, _, _)| id == "a"))
    );
    let requests = requests.lock().unwrap();
    let context = &requests[0].messages;
    assert!(context.iter().any(
        |message| message.tool_result().is_some_and(
            |(id, value, _)| id == "a" && value["error"]["code"] == "execution_interrupted"
        )
    ));
    assert!(context.iter().any(|message| {
        message
            .text_content()
            .contains("Previous execution was interrupted")
    }));
    let info = result.model_context.as_ref().unwrap();
    assert_eq!(info.initial_history_messages, 5);
    assert_eq!(info.initial_context_messages, 7);
    assert_eq!(info.last_request_messages, Some(8));
    assert_eq!(info.requests, 1);
    assert_eq!(info.source.as_ref().unwrap().recorded_messages, 4);
    assert_eq!(info.source.as_ref().unwrap().recovered_tool_results, 1);
    assert_eq!(info.source.as_ref().unwrap().interrupted_turns, 1);
    assert_eq!(
        recorded
            .chat_messages()
            .iter()
            .filter(|message| message.text_content() == "redirect")
            .count(),
        1
    );
}
#[test]
fn live_context_requires_a_paired_active_turn_and_legacy_boundaries_are_repaired() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    repo.append_entries(
        &mut session,
        vec![
            data(calls(&["legacy"])),
            SessionEntryData::TurnStart {
                turn_id: "live".into(),
            },
            data(user("fresh")),
        ],
    )
    .unwrap();
    let branch = session.selected_branch();
    let context = branch.context_for_active_turn("live").unwrap();
    assert_eq!(context.source.recovered_tool_results, 1);
    assert_eq!(context.source.interrupted_turns, 0);
    assert!(matches!(
        context.origins[1],
        ContextOrigin::RecoveredToolResult {
            call_id: "legacy",
            ..
        }
    ));
    assert!(branch.context_for_active_turn("foreign").is_err());
    repo.append(&mut session, calls(&["current"])).unwrap();
    assert!(
        session
            .selected_branch()
            .context_for_active_turn("live")
            .is_err()
    );
    assert_eq!(
        session
            .selected_branch()
            .model_context()
            .unwrap()
            .source
            .interrupted_turns,
        1
    );
    repo.append(&mut session, tool("current")).unwrap();
    assert!(
        session
            .selected_branch()
            .context_for_active_turn("live")
            .is_ok()
    );
}
#[tokio::test]
async fn invalid_historical_tool_units_fail_before_a_new_turn_is_persisted() {
    for messages in [
        vec![tool("orphan")],
        vec![calls(&["a", "a"])],
        vec![calls(&["a"]), tool("unknown")],
        vec![calls(&["a"]), tool("a"), tool("a")],
    ] {
        let root = tempfile::tempdir().unwrap();
        let repo = MemorySessionRepository::new();
        let mut session = repo.create(root.path()).unwrap();
        for message in messages {
            repo.append(&mut session, message).unwrap();
        }
        let before = session.metadata().clone();
        assert!(session.selected_branch().model_context().is_err());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let agent = Agent::new(
            TurnExecutor::builder(Recorder(requests.clone()))
                .workspace(root.path())
                .build()
                .unwrap(),
            AgentConfig::new("offline"),
        );
        let outcome = agent
            .run_with_session_outcome(
                &repo,
                SessionSelection::Existing(before.id.clone()),
                "new",
                Arc::new(()),
                TurnExecutionContext::new(),
            )
            .await;
        assert!(matches!(outcome, AgentTurnOutcome::Failed(_)));
        assert!(requests.lock().unwrap().is_empty());
        assert_eq!(repo.open(&before.id).unwrap().metadata(), &before);
    }
}
struct Prefix;
#[async_trait]
impl InputContextProvider for Prefix {
    async fn prepare(
        &self,
        _: &std::path::Path,
        history: &[Message],
    ) -> anyhow::Result<PreparedInput> {
        assert!(
            !history
                .iter()
                .any(|message| message.text_content().contains("HISTORICAL_MARKER"))
        );
        Ok(PreparedInput {
            system_prefix: vec![Message::text(MessageRole::System, "loaded instructions")],
            provenance: json!({"fixture":true}),
        })
    }
}
#[tokio::test]
async fn explicit_prefix_replacement_preserves_derived_system_markers_and_raw_inputs() {
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let raw = vec![user("full recorded history"), user("latest")];
    let model = vec![
        Message::text(MessageRole::System, "HISTORICAL_MARKER"),
        user("latest"),
    ];
    let input =
        ExecutionInput::projected(ModelRequest::new("offline", model), raw.clone(), 0).unwrap();
    let exec = TurnExecutor::builder(Recorder(requests.clone()))
        .workspace(root.path())
        .input_context(Arc::new(Prefix))
        .build()
        .unwrap();
    let result = exec.run_input(input).await.unwrap();
    assert_eq!(&result.messages[1..3], raw.as_slice());
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests[0].messages[0].text_content(),
        "loaded instructions"
    );
    assert_eq!(requests[0].messages[1].text_content(), "HISTORICAL_MARKER");
    assert!(
        !result
            .messages
            .iter()
            .any(|message| message.text_content().contains("HISTORICAL_MARKER"))
    );
    assert_eq!(result.input_messages, 3);
    assert_eq!(
        result
            .model_context
            .as_ref()
            .unwrap()
            .initial_history_messages,
        3
    );
    assert_eq!(
        result
            .model_context
            .as_ref()
            .unwrap()
            .initial_context_messages,
        3
    );
    assert!(
        ExecutionInput::projected(
            ModelRequest::new("offline", vec![user("x")]),
            vec![Message::text(MessageRole::System, "undeclared")],
            0
        )
        .is_err()
    );
    assert!(
        ExecutionInput::projected(ModelRequest::new("offline", vec![user("x")]), vec![], 1)
            .is_err()
    );
    assert!(
        ExecutionInput::projected(
            ModelRequest::new("offline", vec![Message::text(MessageRole::System, "a")]),
            vec![Message::text(MessageRole::System, "b")],
            1
        )
        .is_err()
    );
}
#[test]
fn completed_turns_cannot_claim_success_with_missing_tool_results() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    let mut record = TurnRecord::recovered();
    record.stop = None;
    repo.append_entries(
        &mut session,
        vec![
            SessionEntryData::TurnStart {
                turn_id: "t".into(),
            },
            data(calls(&["missing"])),
            SessionEntryData::TurnEnd {
                turn_id: "t".into(),
                record: Box::new(record),
            },
        ],
    )
    .unwrap();
    assert!(session.selected_branch().model_context().is_err());
}

#[tokio::test]
async fn context_token_budgets_and_pruning_charge_the_projected_request_not_raw_history() {
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let old = user(&"OLD_DETAIL ".repeat(4000));
    let input = ExecutionInput::projected(
        ModelRequest::new("offline", vec![user("latest")]),
        vec![old.clone(), user("latest")],
        0,
    )
    .unwrap();
    let exec = TurnExecutor::builder(Recorder(requests.clone()))
        .workspace(root.path())
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_tokens: Some(100),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let result = exec.run_input(input).await.unwrap();
    assert_eq!(result.messages[0], old);
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests[0].messages, vec![user("latest")]);
        assert_eq!(
            result
                .model_context
                .as_ref()
                .unwrap()
                .last_input_token_estimate,
            Some(aicoder_core::context::estimate_context_tokens(
                &requests[0].messages,
                Some(&requests[0].tools)
            ) as u64)
        );
    }

    let requests = Arc::new(Mutex::new(Vec::new()));
    let exec = TurnExecutor::builder(Recorder(requests.clone()))
        .workspace(root.path())
        .context_compactor(
            PruningContextCompactor::new(ContextWindowConfig {
                max_context_tokens: 200,
                reserved_output_tokens: 40,
                preserve_recent_tokens: 30,
            })
            .unwrap(),
        )
        .build()
        .unwrap();
    let result = exec
        .run(ModelRequest::new(
            "offline",
            vec![old.clone(), user("latest")],
        ))
        .await
        .unwrap();
    assert_eq!(result.messages[0], old);
    assert_eq!(result.model_context.as_ref().unwrap().compactions, 1);
    assert!(
        !requests.lock().unwrap()[0]
            .messages
            .iter()
            .any(|message| message.text_content().contains("OLD_DETAIL"))
    );
}
#[tokio::test]
async fn capability_filtering_changes_only_the_provider_view_and_report_counts() {
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let reasoning = Message {
        role: MessageRole::Assistant,
        content: vec![ContentBlock::Reasoning {
            text: "private reasoning".into(),
        }],
    };
    let exec = TurnExecutor::builder(Recorder(requests.clone()))
        .workspace(root.path())
        .build()
        .unwrap();
    let result = exec
        .run(ModelRequest::new(
            "offline",
            vec![user("first"), reasoning.clone(), user("next")],
        ))
        .await
        .unwrap();
    assert_eq!(result.messages[1], reasoning);
    assert_eq!(
        result
            .model_context
            .as_ref()
            .unwrap()
            .initial_context_messages,
        3
    );
    assert_eq!(
        result.model_context.as_ref().unwrap().last_request_messages,
        Some(2)
    );
    assert_eq!(
        requests.lock().unwrap()[0].messages,
        vec![user("first"), user("next")]
    );
}

#[test]
fn terminal_context_metadata_is_optional_for_legacy_data_but_consistent_when_present() {
    let frames = include_str!("../../../docs/protocol/events-v1-trace.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<AgentRawEventEnvelope>(line).unwrap())
        .collect::<Vec<_>>();
    for mode in 0..4 {
        let mut altered = frames.clone();
        let summary = Arc::make_mut(altered.last_mut().unwrap().termination.as_mut().unwrap());
        match mode {
            0 => summary.model_context = None,
            1 => summary.model_context.as_mut().unwrap().requests += 1,
            2 => {
                summary
                    .model_context
                    .as_mut()
                    .unwrap()
                    .source
                    .as_mut()
                    .unwrap()
                    .session_id = "foreign".into()
            }
            _ => {
                summary
                    .model_context
                    .as_mut()
                    .unwrap()
                    .last_request_messages = None
            }
        }
        let mut validator = events::EventTraceValidator::new();
        let rejected = altered
            .iter()
            .any(|frame| validator.observe(frame).is_err());
        if mode == 0 {
            assert!(!rejected);
            validator.finish().unwrap();
        } else {
            assert!(rejected);
            assert!(validator.finish().is_err());
        }
    }
}

#[test]
fn reordered_and_interrupted_tool_units_keep_images_with_their_recorded_origin() {
    let root = tempfile::tempdir().unwrap();
    let repo = MemorySessionRepository::new();
    let mut session = repo.create(root.path()).unwrap();
    let mut image_result = tool("b");
    image_result.content.push(ContentBlock::Image {
        source: ImageSource::Artifact {
            reference: aicoder_core::artifacts::ArtifactRef {
                version: 1,
                id: "00000000-0000-0000-0000-000000000001".into(),
                media_type: "image/png".into(),
                bytes: 3,
                complete: true,
            },
        },
    });
    repo.append_entries(
        &mut session,
        vec![
            data(user("inspect")),
            data(calls(&["a", "b"])),
            data(image_result.clone()),
            data(tool("a")),
        ],
    )
    .unwrap();
    let before = serde_json::to_string(session.entries()).unwrap();
    let view = session.selected_branch().model_context().unwrap();
    assert_eq!(view.source.reordered_tool_results, 2);
    assert_eq!(view.messages[3], image_result);
    assert!(
        matches!(view.origins[3], ContextOrigin::Recorded { entry_id } if entry_id==session.entries()[2].id)
    );
    assert_eq!(before, serde_json::to_string(session.entries()).unwrap());
    let mut missing = repo.create(root.path()).unwrap();
    repo.append_entries(
        &mut missing,
        vec![
            data(user("inspect")),
            data(calls(&["a", "b"])),
            data(image_result.clone()),
        ],
    )
    .unwrap();
    let recovered = missing.selected_branch().model_context().unwrap();
    assert_eq!(recovered.source.recovered_tool_results, 1);
    assert_eq!(recovered.messages[3], image_result);
    assert_eq!(recovered.messages[2].content.len(), 1);
    let mut ambiguous = tool("a");
    ambiguous.content.extend(image_result.content.clone());
    let mut invalid = repo.create(root.path()).unwrap();
    repo.append_entries(
        &mut invalid,
        vec![data(calls(&["a", "b"])), data(ambiguous)],
    )
    .unwrap();
    assert!(invalid.selected_branch().model_context().is_err());
}
