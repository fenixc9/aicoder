use aicoder_core::{events::*, model::*, session::*, tools::*, *};
use aicoder_runtime::{
    code_mode::CodeModeSettings,
    exports::{self, ExportFormat, ExportLimits},
    resources::ResourceSettings,
    session::{JsonlSessionRepository, SessionLoadLimits},
    *,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
struct Provider {
    mode: bool,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    requests: Mutex<Vec<ModelRequest>>,
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.tools = true;
        spec.context_tokens = Some(65536);
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let n = self.requests.lock().unwrap().len() + 1;
        self.requests.lock().unwrap().push(request.clone());
        let tools = n == 1;
        if tools {
            self.started.notify_one();
            self.release.notified().await;
        }
        let call = if self.mode {
            ModelToolCall {
                id: "program".into(),
                name: "codemode".into(),
                arguments: json!({"code":"await tools.write_file({path:'result.txt',content:'approved'});text('tool complete');"}),
            }
        } else {
            ModelToolCall {
                id: "write".into(),
                name: "write_file".into(),
                arguments: json!({"path":"result.txt","content":"approved"}),
            }
        };
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: if tools {
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall { call }],
                }
            } else {
                Message::text(MessageRole::Assistant, "final")
            },
            stop_reason: if tools {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage {
                input_tokens: Some(5),
                output_tokens: Some(2),
                total_tokens: Some(7),
                cached_input_tokens: Some(0),
                cache_write_tokens: Some(0),
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<AgentRawEventEnvelope>>);
impl AgentEventHandler for Events {
    fn on_raw_event(&self, event: &AgentRawEventEnvelope) {
        self.0.lock().unwrap().push(event.clone());
    }
}
pub async fn run(mode: bool, allow: bool, delivery: CustomDelivery) -> Result<Value> {
    ensure!(
        !mode || cfg!(feature = "code-mode"),
        "enable code-mode for this variant"
    );
    let root = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let runtime = Runtime::load_with_environment(
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                code_mode: CodeModeSettings {
                    enabled: Setting::Value(mode),
                    only: Setting::Value(mode),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Value(state.path().join("sessions")),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(std::collections::BTreeMap::new()),
    )?;
    let provider = Arc::new(Provider {
        mode,
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        requests: Mutex::new(vec![]),
    });
    let builder = runtime.executor_builder(provider.clone())?;
    let builder = if allow {
        builder.approval(AllowAllApproval)
    } else {
        builder.approval(DenyAllApproval)
    };
    let agent = Arc::new(Agent::new(builder.build()?, AgentConfig::new("offline")));
    let repo = runtime.sessions()?;
    let session = repo.create(root.path())?;
    let id = session.metadata().id.clone();
    let events = Arc::new(Events::default());
    let (handle, control) =
        agent.control_channel(TurnExecutionContext::new(), RunControlConfig::default())?;
    let owner = agent.clone();
    let task_repo = repo.clone();
    let task_id = id.clone();
    let trace = events.clone();
    let run = tokio::spawn(async move {
        owner
            .run_with_session_controlled(
                &task_repo,
                SessionSelection::Existing(task_id),
                "first explicit trigger",
                trace,
                control,
            )
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.started.notified(),
    )
    .await?;
    handle
        .submit_custom(
            delivery,
            CustomMessage::text(
                "sender",
                "queued",
                "QUEUED_CUSTOM_FACT <script>inert</script>",
            )?
            .with_details(json!({"inert":"DETAILS_NOT_FOR_MODEL"}))?,
        )
        .await?;
    provider.release.notify_one();
    let first = run.await?.into_result()?;
    ensure!(
        root.path().join("result.txt").exists() == (allow && delivery != CustomDelivery::Steer),
        "custom delivery bypassed or repeated owned mutation"
    );
    let first_calls = if delivery == CustomDelivery::FollowUp {
        3
    } else {
        2
    };
    ensure!(
        provider.requests.lock().unwrap().len() == first_calls,
        "wrong current-run continuation count"
    );
    ensure!(
        first.execution_result.usage.total_tokens == Some(first_calls as u64 * 7),
        "parent usage changed"
    );
    if delivery == CustomDelivery::NextTurn {
        ensure!(
            agent.custom_inbox().pending(&id).len() == 1,
            "next-turn message not retained"
        );
        ensure!(
            !serde_json::to_string(repo.open(&id)?.entries())?.contains("QUEUED_CUSTOM_FACT"),
            "next-turn context was published early"
        );
        agent
            .run_with_session(
                &repo,
                SessionSelection::Existing(id.clone()),
                "second explicit trigger",
                Arc::new(()),
            )
            .await?;
        ensure!(
            agent.custom_inbox().pending(&id).is_empty(),
            "committed next-turn claim not consumed"
        );
    }
    let requests = provider.requests.lock().unwrap();
    let visible = if matches!(
        delivery,
        CustomDelivery::NextTurn | CustomDelivery::FollowUp
    ) {
        2
    } else {
        1
    };
    ensure!(
        serde_json::to_string(&requests[visible].messages)?.contains("QUEUED_CUSTOM_FACT"),
        "message did not reach requested boundary"
    );
    for request in &requests[..visible] {
        ensure!(
            !serde_json::to_string(&request.messages)?.contains("QUEUED_CUSTOM_FACT"),
            "message was visible before requested delivery"
        );
    }
    ensure!(
        !serde_json::to_string(&*requests)?.contains("DETAILS_NOT_FOR_MODEL"),
        "inert details entered provider view"
    );
    let saved = repo.open(&id)?;
    let custom = saved
        .messages()
        .iter()
        .filter(|m| m.message.role == MessageRole::Custom)
        .count();
    let users = saved
        .messages()
        .iter()
        .filter(|m| m.message.role == MessageRole::User)
        .count();
    ensure!(
        custom == 1
            && users
                == if delivery == CustomDelivery::NextTurn {
                    2
                } else {
                    1
                },
        "queued custom message became a user fact or repeated"
    );
    let raw = events.0.lock().unwrap();
    let mut validator = EventTraceValidator::new();
    for event in raw.iter() {
        validator.observe(event)?;
    }
    validator.finish()?;
    let archive = JsonlSessionRepository::with_limits(
        runtime.session_dir()?,
        SessionLoadLimits {
            max_snapshot_bytes: 1024,
            max_window_messages: 3,
            ..Default::default()
        },
    )?;
    let window = archive.open_run(&id)?;
    ensure!(
        matches!(window, RunSession::Window(_)),
        "fixture did not load archive window"
    );
    let (_, context) = window.source_and_context()?;
    ensure!(
        !serde_json::to_string(&context)?.contains("DETAILS_NOT_FOR_MODEL"),
        "archive metadata became model instructions"
    );
    let export = exports::prepare(
        &repo,
        root.path(),
        &id,
        ExportFormat::Html,
        ExportLimits::default(),
        &TurnExecutionContext::new(),
    )?;
    let html = std::str::from_utf8(export.bytes())?;
    ensure!(
        html.contains("QUEUED_CUSTOM_FACT") && !html.contains("<script>inert</script>"),
        "queued custom export omitted or activated text"
    );
    Ok(
        json!({"code_mode":mode,"approved":allow,"delivery":delivery,"current_run_requests":first_calls,"total_requests":requests.len(),"current_usage":first.execution_result.usage.total_tokens,"custom_messages":custom,"user_messages":users,"inert_details_not_prompted":true,"notifications_valid":true,"bounded_archive_verified":true,"export_inert":true,"network_requests":0}),
    )
}
