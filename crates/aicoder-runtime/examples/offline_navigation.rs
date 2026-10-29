//! Branch metadata, search and validated model changes over the shared runtime and JSONL store.
use aicoder_core::{model::*, session::*, *};
use aicoder_runtime::{
    ConfigDiscovery, ModelSettings, Runtime, RuntimeOverrides, Setting, Settings,
    resources::ResourceSettings,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, Mutex},
};
struct Offline(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl ModelProvider for Offline {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.context_tokens = Some(32768);
        spec.max_output_tokens = Some(512);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        if request.model == "beta" {
            assert!(
                !request
                    .messages
                    .iter()
                    .any(|m| m.text_content().contains("original request"))
            );
        }
        self.0.lock().unwrap().push(request.model.clone());
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                model: Setting::Value("alpha".into()),
                options: ModelSettings {
                    max_output_tokens: Setting::Value(512),
                    ..Default::default()
                },
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::from([(
            "AICODER_HOME".into(),
            state.path().to_string_lossy().into_owned(),
        )])),
    )?;
    let repository = runtime.sessions()?;
    let mut session = repository.create(workspace.path())?;
    repository.append(
        &mut session,
        Message::text(MessageRole::User, "needle common"),
    )?;
    let common = session.metadata().leaf_id.clone().unwrap();
    let id = session.metadata().id.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        runtime
            .executor_builder(Arc::new(Offline(requests.clone())))?
            .build()?,
        runtime.agent_config(),
    )
    .following_session_models();
    let original = agent
        .run_with_session(
            &repository,
            SessionSelection::Existing(id.clone()),
            "original request",
            Arc::new(()),
        )
        .await?;
    let original = original.session.unwrap().leaf_id.unwrap();
    let navigator = SessionNavigator::new(&repository, workspace.path())?;
    navigator.label(&id, &original, Some("stable"))?;
    navigator.branch(&id, Some(&common), Some("experiment"))?;
    let report = agent
        .switch_session_model(&repository, &id, "beta", TurnExecutionContext::new())
        .await?;
    ensure!(report.spec.id == "beta");
    let frames = Arc::new(Mutex::new(Vec::new()));
    let captured = frames.clone();
    agent
        .run_with_session_outcome(
            &repository,
            SessionSelection::Existing(id.clone()),
            "experiment request",
            Arc::new(move |frame: &AgentRawEventEnvelope| {
                captured.lock().unwrap().push(frame.clone())
            }),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    navigator.rename(&id, "navigation demo")?;
    let saved = repository.open(&id)?;
    ensure!(saved.selected_model() == Some(("offline", "beta")));
    ensure!(
        saved
            .search_messages("original request", 10, None)?
            .hits
            .len()
            == 1
    );
    ensure!(saved.labels().len() == 2);
    let first = saved.tree_page(2, None)?;
    ensure!(first.rows.len() == 2 && first.next.is_some());
    navigator.checkout(&id, Some("stable"))?;
    let restored = repository.open(&id)?;
    ensure!(restored.selected_model() == Some(("offline", "alpha")));
    ensure!(restored.metadata().title.as_deref() == Some("navigation demo"));
    agent
        .run_with_session(
            &repository,
            SessionSelection::Existing(id),
            "resume stable",
            Arc::new(()),
        )
        .await?;
    ensure!(*requests.lock().unwrap() == ["alpha", "beta", "alpha"]);
    let frames = frames.lock().unwrap();
    let mut validator = events::EventTraceValidator::new();
    for frame in frames.iter() {
        validator.observe(frame)?;
    }
    validator.finish()?;
    ensure!(
        matches!(&frames[0].event,AgentRawEvent::AgentStarted {model} if model.as_ref()=="beta")
    );
    if let Some(path) = std::env::args().nth(1) {
        fs::write(
            path,
            frames
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()?
                .join("\n")
                + "\n",
        )?;
    }
    println!(
        "{}",
        json!({"status":"completed","models":*requests.lock().unwrap(),"rename_survives_checkout":true,"inactive_branch_searchable":true,"branch_model_restored":true,"tree_page_rows":first.rows.len(),"event_frames":frames.len(),"model_selection":report})
    );
    Ok(())
}
