use super::*;
use aicoder_core::auxiliary::*;
use aicoder_providers::{
    ClassifierImageInput, StaticCredential, auxiliary::SystemOneClient,
    image_generation::OpenRouterImageClient,
};
fn classifier_model() -> ClassifierModel {
    ClassifierModel {
        provider: "fixture".into(),
        id: "m".into(),
        name: "classifier".into(),
        images: true,
        context_tokens: 65536,
        output_reservation: 1024,
    }
}
fn image_model() -> ImageGenerationModel {
    ImageGenerationModel {
        specification: classifier_model(),
        text_output: true,
    }
}
fn classifier_context(images: bool) -> ClassifierContext {
    serde_json::from_value(json!({"state":{"text":"sample"},"questions":{"ok":{"type":"bool","instructions":"valid?","criteria":{"true":"yes","false":"no"}}},"images":if images{json!([{"type":"base64","media_type":"image/png","data":"AAAA"}])}else{json!([])}})).unwrap()
}
fn image_context() -> ImageGenerationContext {
    ImageGenerationContext {
        input: vec![ImageGenerationInput::Text {
            text: "Paint".into(),
        }],
    }
}
fn classifier(url: String, credentials: Arc<dyn CredentialProvider>) -> SystemOneClient {
    SystemOneClient::new(
        config(url, 3),
        credentials,
        vec![classifier_model()],
        ClassifierImageInput::Assets,
    )
    .unwrap()
}
fn image(url: String, credentials: Arc<dyn CredentialProvider>) -> OpenRouterImageClient {
    OpenRouterImageClient::new(config(url, 3), credentials, vec![image_model()]).unwrap()
}
fn answer() -> Value {
    json!({"answers":{"ok":{"type":"noul","noul":0.9}},"usage":{"input_tokens":9,"output_tokens":2}})
}
fn picture() -> Value {
    let mut value = success();
    value["choices"][0]["message"]["images"] =
        json!([{"image_url":{"url":"data:image/png;base64,AAAA"}}]);
    value
}
#[tokio::test]
async fn native_clients_share_preparation_observation_and_reports_without_changing_typed_usage() {
    let (url, server) = server(vec![
        Response {
            status: "200 OK",
            body: answer().to_string(),
            stream: false,
            short: false,
        },
        Response {
            status: "200 OK",
            body: picture().to_string(),
            stream: false,
            short: false,
        },
    ])
    .await;
    let state = Arc::new(State::default());
    let binding = TransportBinding::new(hooks(state.clone(), true));
    let credentials: Arc<dyn CredentialProvider> =
        Arc::new(StaticCredential::new("fixture-key").unwrap());
    let judge =
        classifier(url.clone(), credentials.clone()).with_transport_binding(binding.clone());
    let reporter = AuxiliaryUsageReporter::default();
    let answers = judge
        .classify(
            &classifier_model(),
            &classifier_context(true),
            reporter.clone(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert!(matches!(answers["ok"],ClassifierAnswer::Bool{probability}if probability==0.9));
    assert_eq!(reporter.usage().unwrap().total_tokens, Some(11));
    let painter = image(url, credentials).with_transport_binding(binding.clone());
    let reporter = AuxiliaryUsageReporter::default();
    let output = painter
        .generate_images(
            &image_model(),
            &image_context(),
            reporter.clone(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(reporter.usage().unwrap().total_tokens, Some(11));
    assert!(matches!(&output[1],GeneratedBlock::Image{bytes,..}if bytes.len()==3));
    let captured = server.await.unwrap();
    assert_eq!(captured.len(), 2);
    assert!(captured[0].0.starts_with("POST /v1/systemone "));
    assert!(captured[1].0.starts_with("POST /v1/chat/completions "));
    assert!(
        captured[0]
            .0
            .to_lowercase()
            .contains("typesafe-danger-zone-images: true")
    );
    assert_eq!(captured[0].1["questions"]["ok"]["type"], "noul");
    assert_eq!(captured[0].1["assets"]["<image-1>"]["data_base64"], "AAAA");
    assert_eq!(captured[1].1["modalities"], json!(["image", "text"]));
    assert_eq!(
        captured[0].1["metadata"]["first"],
        captured[0].1["metadata"]["second"]
    );
    assert_eq!(binding.report().failures.len(), 4);
    assert!(
        binding
            .report()
            .failures
            .iter()
            .all(|row| row.provider == "fixture" && row.model == "m")
    );
    assert_eq!(judge.transport_report().failures.len(), 4);
    assert_eq!(painter.transport_report().failures.len(), 4);
    assert!(state.events.lock().unwrap().is_empty());
}
struct Edit(&'static str);
#[async_trait]
impl TransportHook for Edit {
    async fn before_headers(
        &self,
        _: TransportHeadersInput<'_>,
        _: TransportCancellation,
    ) -> Result<BTreeMap<String, Option<String>>> {
        Ok(if self.0 == "experimental_header" {
            BTreeMap::from([("TypeSafe-Danger-Zone-Images".into(), None)])
        } else {
            BTreeMap::new()
        })
    }
    async fn before_payload(
        &self,
        input: TransportPayloadInput<'_>,
        _: TransportCancellation,
    ) -> Result<Option<Value>> {
        let mut value = input.payload.clone();
        if self.0 != "experimental_header" {
            value[self.0] = json!("forged");
        }
        Ok(Some(value))
    }
}
#[tokio::test]
async fn native_inputs_and_experimental_protocol_cannot_be_edited_after_approval_admission() {
    for field in ["state", "questions", "images", "assets", "modalities"] {
        assert!(TransportHooks::new().allow_payload_field(field).is_err());
    }
    for (field, is_image) in [
        ("state", false),
        ("questions", false),
        ("images", false),
        ("assets", false),
        ("experimental_header", false),
        ("modalities", true),
        ("messages", true),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut hooks = TransportHooks::new();
        hooks
            .register("edit", HookSpec::new("edit"), Arc::new(Edit(field)))
            .unwrap();
        let binding = TransportBinding::new(hooks);
        let reporter = AuxiliaryUsageReporter::default();
        let credentials: Arc<dyn CredentialProvider> = Arc::new(Credential(calls.clone()));
        let result = if is_image {
            image(url, credentials)
                .with_transport_binding(binding.clone())
                .generate_images(
                    &image_model(),
                    &image_context(),
                    reporter.clone(),
                    TurnExecutionContext::new(),
                )
                .await
                .map(|_| ())
        } else {
            classifier(url, credentials)
                .with_transport_binding(binding.clone())
                .classify(
                    &classifier_model(),
                    &classifier_context(true),
                    reporter.clone(),
                    TurnExecutionContext::new(),
                )
                .await
                .map(|_| ())
        };
        assert!(
            matches!(result,Err(ref e)if e.kind==ProviderErrorKind::InvalidRequest),
            "{field}"
        );
        assert_eq!(reporter.usage().unwrap().total_tokens, Some(0));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!binding.report().failures.is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
    }
}
struct WaitHeader {
    started: Arc<tokio::sync::Notify>,
    ended: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl TransportHook for WaitHeader {
    async fn before_headers(
        &self,
        _: TransportHeadersInput<'_>,
        c: TransportCancellation,
    ) -> Result<BTreeMap<String, Option<String>>> {
        let end = self.ended.clone();
        tokio::spawn(async move {
            c.cancelled().await;
            end.notify_one();
        });
        self.started.notify_one();
        std::future::pending().await
    }
}
#[tokio::test]
async fn cancelling_native_preparation_expires_private_children_without_http_or_late_usage() {
    for is_image in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let started = Arc::new(tokio::sync::Notify::new());
        let ended = Arc::new(tokio::sync::Notify::new());
        let mut hooks = TransportHooks::new();
        hooks
            .register(
                "wait",
                HookSpec::new("wait"),
                Arc::new(WaitHeader {
                    started: started.clone(),
                    ended: ended.clone(),
                }),
            )
            .unwrap();
        let binding = TransportBinding::new(hooks);
        let calls = Arc::new(AtomicUsize::new(0));
        let credentials: Arc<dyn CredentialProvider> = Arc::new(Credential(calls.clone()));
        let reporter = AuxiliaryUsageReporter::default();
        let observed = reporter.clone();
        let context = TurnExecutionContext::new();
        let worker = context.clone();
        let task = tokio::spawn(async move {
            if is_image {
                image(url, credentials)
                    .with_transport_binding(binding)
                    .generate_images(&image_model(), &image_context(), reporter, worker)
                    .await
                    .map(|_| ())
            } else {
                classifier(url, credentials)
                    .with_transport_binding(binding)
                    .classify(
                        &classifier_model(),
                        &classifier_context(true),
                        reporter,
                        worker,
                    )
                    .await
                    .map(|_| ())
            }
        });
        started.notified().await;
        context.cancel("cancel native transport");
        assert!(matches!(task.await.unwrap(),Err(ref e)if e.kind==ProviderErrorKind::Cancelled));
        tokio::time::timeout(Duration::from_secs(1), ended.notified())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(observed.usage().is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn native_error_or_incomplete_response_never_retries_with_transport_selected() {
    for is_image in [false, true] {
        for short in [false, true] {
            let (url, server) = server(vec![Response {
                status: if short {
                    "200 OK"
                } else {
                    "503 Service Unavailable"
                },
                body: if short { "{".into() } else { "{}".into() },
                stream: false,
                short,
            }])
            .await;
            let binding = TransportBinding::new(hooks(Arc::new(State::default()), false));
            let credentials: Arc<dyn CredentialProvider> =
                Arc::new(StaticCredential::new("fixture-key").unwrap());
            let reporter = AuxiliaryUsageReporter::default();
            let result = if is_image {
                image(url, credentials)
                    .with_transport_binding(binding)
                    .generate_images(
                        &image_model(),
                        &image_context(),
                        reporter,
                        TurnExecutionContext::new(),
                    )
                    .await
                    .map(|_| ())
            } else {
                classifier(url, credentials)
                    .with_transport_binding(binding)
                    .classify(
                        &classifier_model(),
                        &classifier_context(false),
                        reporter,
                        TurnExecutionContext::new(),
                    )
                    .await
                    .map(|_| ())
            };
            assert!(result.is_err());
            assert_eq!(server.await.unwrap().len(), 1);
        }
    }
}
