//! OpenRouter image modality over chat completions, separate from conversational inference.
use crate::{ClientConfig, CredentialProvider};
use aicoder_core::{TurnExecutionContext, auxiliary::*, model::*};
use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

pub struct OpenRouterImageClient {
    transport_binding: Option<crate::transport::TransportBinding>,
    proxy_secrets: Vec<String>,
    config: ClientConfig,
    http: reqwest::Client,
    endpoint: reqwest::Url,
    credentials: Arc<dyn CredentialProvider>,
    models: Vec<ImageGenerationModel>,
}
impl OpenRouterImageClient {
    pub fn new(
        mut config: ClientConfig,
        credentials: Arc<dyn CredentialProvider>,
        models: Vec<ImageGenerationModel>,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        anyhow::ensure!(
            (1..=64).contains(&models.len()),
            "invalid image model catalog"
        );
        let mut seen = std::collections::BTreeSet::new();
        for model in &models {
            let spec = &model.specification;
            spec.validate()?;
            anyhow::ensure!(
                spec.provider == config.provider_id && seen.insert(&spec.id),
                "image route/catalog mismatch"
            );
        }
        let endpoint = config.endpoint()?;
        config.api_key.clear();
        let builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(config.timeout))
            .connect_timeout(Duration::from_secs(config.connect_timeout));
        let http = config
            .proxy
            .apply(builder)?
            .build()
            .map_err(|_| anyhow::anyhow!("image HTTP client could not be created"))?;
        let proxy_secrets = crate::transport_client::proxy_secrets(&config);
        Ok(Self {
            transport_binding: None,
            proxy_secrets,
            config,
            http,
            endpoint,
            credentials,
            models,
        })
    }
    pub fn with_transport_hooks(self, hooks: crate::transport::TransportHooks) -> Self {
        self.with_transport_binding(crate::transport::TransportBinding::new(hooks))
    }
    pub fn with_shared_transport_hooks(self, hooks: Arc<crate::transport::TransportHooks>) -> Self {
        self.with_transport_binding(crate::transport::TransportBinding::shared(hooks))
    }
    pub fn with_transport_binding(mut self, binding: crate::transport::TransportBinding) -> Self {
        self.transport_binding = binding.active().then_some(binding);
        self
    }
    pub fn transport_hook_catalog(&self) -> Vec<crate::transport::TransportHookInfo> {
        self.transport_binding
            .as_ref()
            .map(|b| b.catalog())
            .unwrap_or_default()
    }
    pub fn transport_payload_fields(&self) -> Vec<String> {
        self.transport_binding
            .as_ref()
            .map(|b| b.payload_fields())
            .unwrap_or_default()
    }
    pub fn transport_report(&self) -> crate::transport::TransportReport {
        self.transport_binding
            .as_ref()
            .map(|b| b.report())
            .unwrap_or_default()
    }
    fn transport_driver(&self) -> crate::transport_client::Driver<'_> {
        crate::transport_client::Driver {
            config: &self.config,
            binding: self.transport_binding.as_ref(),
            proxy_secrets: &self.proxy_secrets,
        }
    }
    fn error(&self, kind: ProviderErrorKind, message: &str) -> ProviderError {
        ProviderError::new(&self.config.provider_id, kind, message)
    }
    fn payload(
        &self,
        model: &ImageGenerationModel,
        context: &ImageGenerationContext,
    ) -> Result<Value, ProviderError> {
        context.validate(model).map_err(|_| {
            self.error(
                ProviderErrorKind::InvalidRequest,
                "invalid image generation context",
            )
        })?;
        let mut content = Vec::new();
        for block in &context.input {
            content.push(match block {
                ImageGenerationInput::Text { text } => json!({"type":"text","text":text}),
                ImageGenerationInput::Image { source: ImageSource::Base64 { media_type,data } } => {
                    let mime=aicoder_core::media::image_media_type(media_type).filter(|m|matches!(m.as_ref(),"image/png"|"image/jpeg"|"image/gif"|"image/webp"))
                        .ok_or_else(||self.error(ProviderErrorKind::Unsupported,"unsupported image input format"))?;
                    json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{data}")}})
                }
                _ => return Err(self.error(ProviderErrorKind::InvalidRequest,"image references must be captured by the host; URLs are not fetched")),
            });
        }
        Ok(
            json!({"model":model.specification.id,"messages":[{"role":"user","content":content}],"stream":false,"modalities":if model.text_output{vec!["image","text"]}else{vec!["image"]}}),
        )
    }
    fn decode(
        &self,
        value: &Value,
        model: &ImageGenerationModel,
    ) -> Result<Vec<GeneratedBlock>, ProviderError> {
        let invalid = || {
            self.error(
                ProviderErrorKind::InvalidResponse,
                "invalid or incomplete generated image response; inference was not retried",
            )
        };
        let choices = value["choices"].as_array().ok_or_else(invalid)?;
        if choices.len() != 1 || choices[0]["finish_reason"] != "stop" {
            return Err(invalid());
        }
        let message = &choices[0]["message"];
        let mut output = Vec::new();
        if !message["content"].is_null() {
            let text = message["content"].as_str().ok_or_else(invalid)?;
            if !text.is_empty() {
                output.push(GeneratedBlock::Text { text: text.into() });
            }
        }
        let images = message["images"].as_array().ok_or_else(invalid)?;
        if images.is_empty() || images.len() > 4 {
            return Err(invalid());
        }
        let mut total = 0usize;
        for image in images {
            let uri = image["image_url"]
                .as_str()
                .or_else(|| image["image_url"]["url"].as_str())
                .ok_or_else(invalid)?;
            let (header, data) = uri.split_once(',').ok_or_else(invalid)?;
            let mime = header
                .strip_prefix("data:")
                .and_then(|h| h.strip_suffix(";base64"))
                .ok_or_else(invalid)?;
            let mime = aicoder_core::media::image_media_type(mime)
                .ok_or_else(invalid)?
                .into_owned();
            if data.len() > MAX_GENERATED_IMAGE_BYTES.div_ceil(3) * 4 {
                return Err(invalid());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| invalid())?;
            total = total.saturating_add(bytes.len());
            if total > MAX_GENERATED_TOTAL_BYTES {
                return Err(invalid());
            }
            output.push(GeneratedBlock::Image {
                media_type: mime,
                bytes,
            });
        }
        validate_generated_output(&output, model).map_err(|_| invalid())?;
        Ok(output)
    }
}
#[async_trait]
impl ImageGenerationProvider for OpenRouterImageClient {
    async fn generate_images(
        &self,
        model: &ImageGenerationModel,
        context: &ImageGenerationContext,
        usage: AuxiliaryUsageReporter,
        cancellation: TurnExecutionContext,
    ) -> Result<Vec<GeneratedBlock>, ProviderError> {
        let run = async {
            let Some(registered) = self.models.iter().find(|m| {
                m.specification.provider == model.specification.provider
                    && m.specification.id == model.specification.id
            }) else {
                usage.record(ModelUsage::zero());
                return Err(self.error(
                    ProviderErrorKind::ModelNotFound,
                    "image model is not in this host route",
                ));
            };
            let payload = match self.payload(registered, context) {
                Ok(v) => v,
                Err(e) => {
                    usage.record(ModelUsage::zero());
                    return Err(e);
                }
            };
            let headers = std::collections::BTreeMap::from([(
                "content-type".into(),
                "application/json".into(),
            )]);
            let driver = self.transport_driver();
            let prepared = match driver.prepare(payload, headers, None).await {
                Ok(v) => v,
                Err(_) => {
                    usage.record(ModelUsage::zero());
                    return Err(self.error(
                        ProviderErrorKind::InvalidRequest,
                        "native transport preparation failed; no provider was invoked",
                    ));
                }
            };
            let token = match self.credentials.bearer_token().await {
                Ok(t) => t,
                Err(_) => {
                    usage.record(ModelUsage::zero());
                    return Err(self.error(
                        ProviderErrorKind::Authentication,
                        "configured image credential is unavailable",
                    ));
                }
            };
            if driver
                .transport_ready(prepared.session.as_ref())
                .await
                .is_err()
            {
                usage.record(ModelUsage::zero());
                return Err(self.error(
                    ProviderErrorKind::InvalidRequest,
                    "native transport admission expired; no provider was invoked",
                ));
            }
            let response = self
                .http
                .post(self.endpoint.clone())
                .header(reqwest::header::AUTHORIZATION, token.header())
                .headers(
                    prepared
                        .headers
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.parse::<reqwest::header::HeaderName>()
                                    .expect("validated header"),
                                v.parse::<reqwest::header::HeaderValue>()
                                    .expect("validated header"),
                            )
                        })
                        .collect(),
                )
                .json(&prepared.payload)
                .send()
                .await
                .map_err(|_| {
                    self.error(
                        ProviderErrorKind::Transport,
                        "image transport failed; completion is uncertain",
                    )
                })?;
            driver
                .observe_transport_response(prepared.session.as_ref(), 1, &response, &token)
                .await;
            if !response.status().is_success() {
                let mut error = self.error(
                    ProviderErrorKind::Other,
                    "image service returned unsuccessful status; no automatic retry",
                );
                error.http_status = Some(response.status().as_u16());
                return Err(error);
            }
            let max = self.config.max_response_bytes.min(24 * 1024 * 1024);
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| {
                    self.error(
                        ProviderErrorKind::Transport,
                        "image response was incomplete",
                    )
                })?;
                if chunk.len() > max.saturating_sub(bytes.len()) {
                    return Err(self.error(
                        ProviderErrorKind::InvalidResponse,
                        "image response byte budget exceeded",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                self.error(
                    ProviderErrorKind::InvalidResponse,
                    "image response is not valid JSON",
                )
            })?;
            if let Some(source) = value.get("usage").filter(|v| v.is_object()) {
                let input = source.get("prompt_tokens").and_then(Value::as_u64);
                let output = source.get("completion_tokens").and_then(Value::as_u64);
                let total = source
                    .get("total_tokens")
                    .and_then(Value::as_u64)
                    .or_else(|| input.and_then(|i| output.and_then(|o| i.checked_add(o))));
                if input.is_some() || output.is_some() || total.is_some() {
                    usage.record(ModelUsage {
                        input_tokens: input,
                        output_tokens: output,
                        total_tokens: total,
                        cached_input_tokens: source["prompt_tokens_details"]["cached_tokens"]
                            .as_u64(),
                        cache_write_tokens: source["prompt_tokens_details"]["cache_write_tokens"]
                            .as_u64(),
                    });
                }
            }
            self.decode(&value, registered)
        };
        tokio::select! { biased; _=cancellation.cancelled()=>Err(self.error(ProviderErrorKind::Cancelled,"image generation cancelled; completion may be uncertain")),result=run=>result }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn client() -> (OpenRouterImageClient, ImageGenerationModel) {
        let model = ImageGenerationModel {
            specification: AuxiliaryModel {
                provider: "painter".into(),
                id: "native".into(),
                name: String::new(),
                images: true,
                context_tokens: 65536,
                output_reservation: 1024,
            },
            text_output: true,
        };
        let client = OpenRouterImageClient::new(
            ClientConfig {
                provider_id: "painter".into(),
                base_url: "https://must-not-contact.invalid/v1".into(),
                ..Default::default()
            },
            Arc::new(crate::StaticCredential::new("fixture-key").unwrap()),
            vec![model.clone()],
        )
        .unwrap();
        (client, model)
    }
    #[test]
    fn native_image_modality_requires_inline_complete_images_without_url_fetches() {
        let (client, model) = client();
        let context = ImageGenerationContext {
            input: vec![
                ImageGenerationInput::Text {
                    text: "paint".into(),
                },
                ImageGenerationInput::Image {
                    source: ImageSource::Base64 {
                        media_type: " IMAGE/JPG ; p=1".into(),
                        data: "AAAA".into(),
                    },
                },
            ],
        };
        let payload = client.payload(&model, &context).unwrap();
        assert_eq!(payload["modalities"], json!(["image", "text"]));
        assert_eq!(
            payload["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/jpeg;base64,AAAA"
        );
        let mut response = json!({"choices":[{"finish_reason":"stop","message":{"images":[{"image_url":{"url":"data:image/png;base64,AAAA"}}]}}]});
        client.decode(&response, &model).unwrap();
        for uri in [
            "https://must-not-fetch.invalid/private",
            "data:image/png;base64,!!!",
            "data:text/plain;base64,AAAA",
            "data:image/png;base64,",
        ] {
            response["choices"][0]["message"]["images"][0]["image_url"]["url"] = json!(uri);
            assert!(client.decode(&response, &model).is_err());
        }
        response["choices"][0]["message"]["images"][0]["image_url"]["url"] =
            json!("data:image/png;base64,AAAA");
        response["choices"][0]["finish_reason"] = json!("length");
        assert!(client.decode(&response, &model).is_err());
    }
}
