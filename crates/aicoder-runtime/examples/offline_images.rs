//! Normalize a local image and run the same immutable user message through the SDK offline.
use aicoder_core::{model::*, session::*, *};
use aicoder_runtime::{images, resources::ResourceSettings, session::JsonlSessionRepository, *};
use async_trait::async_trait;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
struct Provider(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.images = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.0.lock().unwrap().push(request.clone());
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "image received"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let storage = tempfile::tempdir()?;
    image::RgbaImage::from_pixel(60, 40, image::Rgba([200, 50, 20, 255]))
        .save(workspace.path().join("input.png"))?;
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Clear,
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::new()),
    )?;
    let attachment = runtime
        .read_image("input.png", Default::default(), TurnExecutionContext::new())
        .await?;
    let input = images::input(
        "describe the capture".into(),
        std::slice::from_ref(&attachment),
    )?;
    std::fs::write(
        workspace.path().join("input.png"),
        b"replaced after immutable capture",
    )?;
    let calls = Arc::new(Mutex::new(vec![]));
    let agent = Agent::new(
        TurnExecutor::builder(Provider(calls.clone()))
            .workspace(workspace.path())
            .build()?,
        AgentConfig::new("vision"),
    );
    let repo = JsonlSessionRepository::new(storage.path())?;
    let session = repo.create(workspace.path())?;
    agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            input.clone(),
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    let saved = repo.open(&session.metadata().id)?;
    anyhow::ensure!(
        saved.messages()[0].message == input.message(),
        "canonical image changed"
    );
    anyhow::ensure!(
        calls.lock().unwrap()[0].messages.last() == Some(input.message()),
        "provider view changed"
    );
    println!(
        "{}",
        serde_json::json!({"image":attachment.info(),"canonical_replay":true,"provider_projection":true,"network_requests":0})
    );
    Ok(())
}
