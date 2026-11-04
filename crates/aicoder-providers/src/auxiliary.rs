//! Native System One tasks. No chat emulation, redirects or implicit billing retries.
use crate::{ClassifierImageInput, ClientConfig, CredentialProvider};
use aicoder_core::{TurnExecutionContext, auxiliary::*, model::*};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
pub struct SystemOneClient {
    config: ClientConfig,
    http: reqwest::Client,
    endpoint: reqwest::Url,
    credentials: Arc<dyn CredentialProvider>,
    models: Vec<ClassifierModel>,
    images: ClassifierImageInput,
}
impl SystemOneClient {
    pub fn new(
        mut config: ClientConfig,
        credentials: Arc<dyn CredentialProvider>,
        models: Vec<ClassifierModel>,
        images: ClassifierImageInput,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        anyhow::ensure!(
            !models.is_empty() && models.len() <= 64,
            "classifier catalog must contain 1..64 models"
        );
        let mut seen = std::collections::BTreeSet::new();
        for model in &models {
            model.validate()?;
            anyhow::ensure!(
                model.provider == config.provider_id && seen.insert(&model.id),
                "classifier route/catalog mismatch"
            );
        }
        let mut endpoint = config.endpoint()?;
        endpoint.set_path(&format!(
            "{}/systemone",
            config
                .base_url
                .trim_end_matches('/')
                .parse::<reqwest::Url>()?
                .path()
                .trim_end_matches('/')
        ));
        config.api_key.clear();
        let builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(config.timeout))
            .connect_timeout(Duration::from_secs(config.connect_timeout));
        let http = config
            .proxy
            .apply(builder)?
            .build()
            .map_err(|_| anyhow::anyhow!("classifier HTTP client could not be created"))?;
        Ok(Self {
            config,
            http,
            endpoint,
            credentials,
            models,
            images,
        })
    }
    fn error(&self, kind: ProviderErrorKind, message: &str) -> ProviderError {
        ProviderError::new(&self.config.provider_id, kind, message)
    }
    fn payload(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
    ) -> Result<Value, ProviderError> {
        context.validate(model).map_err(|_| {
            self.error(
                ProviderErrorKind::InvalidRequest,
                "invalid classifier context",
            )
        })?;
        let mut questions = serde_json::to_value(&context.questions).map_err(|_| {
            self.error(
                ProviderErrorKind::InvalidRequest,
                "invalid classifier questions",
            )
        })?;
        for value in questions
            .as_object_mut()
            .expect("question map")
            .values_mut()
        {
            if value["type"] == "bool" {
                value["type"] = json!("noul");
            }
        }
        let mut payload = json!({"model":model.id,"state":context.state,"questions":questions});
        let mut images = Vec::new();
        for source in &context.images {
            let ImageSource::Base64 { media_type, data } = source else {
                return Err(self.error(
                    ProviderErrorKind::InvalidRequest,
                    "classifier image references must be resolved by the host",
                ));
            };
            let mime = aicoder_core::media::image_media_type(media_type)
                .ok_or_else(|| {
                    self.error(
                        ProviderErrorKind::InvalidRequest,
                        "invalid classifier image MIME",
                    )
                })?
                .into_owned();
            if !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") {
                return Err(self.error(
                    ProviderErrorKind::Unsupported,
                    "classifier image format is unsupported",
                ));
            }
            images.push((mime, data));
        }
        if !images.is_empty() {
            match self.images {
                ClassifierImageInput::Assets => {
                    let mut assets = serde_json::Map::new();
                    let mut state = Vec::new();
                    for (index, (mime, data)) in images.iter().enumerate() {
                        let format = match mime.as_str() {
                            "image/png" => "png",
                            "image/jpeg" => "jpeg",
                            _ => {
                                return Err(self.error(
                                    ProviderErrorKind::Unsupported,
                                    "asset classifier accepts PNG/JPEG only",
                                ));
                            }
                        };
                        let key = format!("<image-{}>", index + 1);
                        state.push(json!(key));
                        assets.insert(
                            key,
                            json!({"type":"image","format":format,"data_base64":data}),
                        );
                    }
                    state.push(payload["state"].take());
                    payload["state"] = json!(state);
                    payload["assets"] = Value::Object(assets);
                }
                ClassifierImageInput::StateImageUrls => {
                    let mut state=images.iter().map(|(mime,data)|json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{data}")}})).collect::<Vec<_>>();
                    state.push(payload["state"].take());
                    payload["state"] = json!(state);
                }
                ClassifierImageInput::Images => {
                    payload["images"] = json!(
                        images
                            .iter()
                            .map(|(mime, data)| format!("data:{mime};base64,{data}"))
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
        Ok(payload)
    }
}
#[async_trait]
impl ClassifierProvider for SystemOneClient {
    async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        usage: AuxiliaryUsageReporter,
        cancellation: TurnExecutionContext,
    ) -> Result<ClassifierAnswers, ProviderError> {
        let run = async {
            let Some(registered) = self
                .models
                .iter()
                .find(|m| m.provider == model.provider && m.id == model.id)
            else {
                usage.record(ModelUsage::zero());
                return Err(self.error(
                    ProviderErrorKind::ModelNotFound,
                    "classifier is not in this host route",
                ));
            };
            let payload = match self.payload(registered, context) {
                Ok(payload) => payload,
                Err(error) => {
                    usage.record(ModelUsage::zero());
                    return Err(error);
                }
            };
            let token = match self.credentials.bearer_token().await {
                Ok(token) => token,
                Err(_) => {
                    usage.record(ModelUsage::zero());
                    return Err(self.error(
                        ProviderErrorKind::Authentication,
                        "configured classifier credential is unavailable",
                    ));
                }
            };
            let mut request = self
                .http
                .post(self.endpoint.clone())
                .header(reqwest::header::AUTHORIZATION, token.header())
                .json(&payload);
            if !context.images.is_empty() && matches!(self.images, ClassifierImageInput::Assets) {
                request = request.header("TypeSafe-Danger-Zone-Images", "true");
            }
            let response = request.send().await.map_err(|_| {
                self.error(
                    ProviderErrorKind::Transport,
                    "classifier transport failed; request completion is uncertain",
                )
            })?;
            if !response.status().is_success() {
                let mut error = self.error(
                    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                        ProviderErrorKind::Authentication
                    } else {
                        ProviderErrorKind::Other
                    },
                    "classifier service returned an unsuccessful status; no automatic retry",
                );
                error.http_status = Some(response.status().as_u16());
                return Err(error);
            }
            let maximum = self.config.max_response_bytes.min(2 * 1024 * 1024);
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| {
                    self.error(
                        ProviderErrorKind::Transport,
                        "classifier response was incomplete",
                    )
                })?;
                if chunk.len() > maximum.saturating_sub(bytes.len()) {
                    return Err(self.error(
                        ProviderErrorKind::InvalidResponse,
                        "classifier response byte budget exceeded",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                self.error(
                    ProviderErrorKind::InvalidResponse,
                    "classifier response is not valid JSON",
                )
            })?;
            if let Some(source) = value.get("usage").filter(|v| v.is_object()) {
                let input = source.get("input_tokens").and_then(Value::as_u64);
                let output = source.get("output_tokens").and_then(Value::as_u64);
                if input.is_some() || output.is_some() {
                    usage.record(ModelUsage {
                        input_tokens: input,
                        output_tokens: output,
                        total_tokens: input.and_then(|i| output.and_then(|o| i.checked_add(o))),
                        cached_input_tokens: Some(0),
                        cache_write_tokens: Some(0),
                    });
                }
            }
            let answers = value
                .get_mut("answers")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    self.error(
                        ProviderErrorKind::InvalidResponse,
                        "classifier response has no answer map",
                    )
                })?;
            for answer in answers.values_mut() {
                if answer["type"] == "noul" {
                    answer["type"] = json!("bool");
                    answer["probability"] = answer["noul"].take();
                    if let Some(map) = answer.as_object_mut() {
                        map.remove("noul");
                    }
                }
            }
            let answers: ClassifierAnswers = serde_json::from_value(Value::Object(answers.clone()))
                .map_err(|_| {
                    self.error(
                        ProviderErrorKind::InvalidResponse,
                        "classifier answer shape is invalid",
                    )
                })?;
            validate_answers(&answers, context).map_err(|_| {
                self.error(
                    ProviderErrorKind::InvalidResponse,
                    "classifier answer values are invalid",
                )
            })?;
            Ok(answers)
        };
        tokio::select! {biased;_ = cancellation.cancelled()=>Err(self.error(ProviderErrorKind::Cancelled,"classifier request cancelled; completion may be uncertain")),result=run=>result}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model() -> ClassifierModel {
        ClassifierModel {
            provider: "fixture".into(),
            id: "native".into(),
            name: "native".into(),
            images: true,
            context_tokens: 65536,
            output_reservation: 1024,
        }
    }
    fn context() -> ClassifierContext {
        serde_json::from_value(json!({"state":{"text":"data"},"questions":{"ok":{"type":"bool","instructions":"valid?","criteria":{"true":"yes","false":"no"}}},"images":[{"type":"base64","media_type":"IMAGE/JPG; profile=fixture","data":"AQID"}]})).unwrap()
    }
    #[test]
    fn native_image_encodings_are_explicit_and_urls_or_wrong_model_types_never_fall_back_to_chat() {
        for mode in [
            ClassifierImageInput::Assets,
            ClassifierImageInput::StateImageUrls,
            ClassifierImageInput::Images,
        ] {
            let client = SystemOneClient::new(
                ClientConfig {
                    provider_id: "fixture".into(),
                    base_url: "http://127.0.0.1:1/v1".into(),
                    api_key_env: Some("FIXTURE_KEY".into()),
                    ..Default::default()
                },
                Arc::new(crate::StaticCredential::new("fixture-only").unwrap()),
                vec![model()],
                mode,
            )
            .unwrap();
            let body = client.payload(&model(), &context()).unwrap();
            assert_eq!(body["questions"]["ok"]["type"], "noul");
            match mode {
                ClassifierImageInput::Assets => {
                    assert_eq!(body["assets"]["<image-1>"]["format"], "jpeg");
                    assert_eq!(body["state"][0], "<image-1>");
                }
                ClassifierImageInput::StateImageUrls => assert_eq!(
                    body["state"][0]["image_url"]["url"],
                    "data:image/jpeg;base64,AQID"
                ),
                ClassifierImageInput::Images => {
                    assert_eq!(body["images"][0], "data:image/jpeg;base64,AQID")
                }
            }
            let mut unsupported = model();
            unsupported.images = false;
            assert!(client.payload(&unsupported, &context()).is_err());
            let mut source = context();
            source.images = vec![ImageSource::Url {
                url: "https://must-not-fetch.invalid".into(),
            }];
            assert!(client.payload(&model(), &source).is_err());
        }
    }
}
