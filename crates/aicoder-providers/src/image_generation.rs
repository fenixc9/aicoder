//! OpenRouter image modality over chat completions, separate from conversational inference.
use crate::{ClientConfig, CredentialProvider};
use aicoder_core::{TurnExecutionContext, auxiliary::*, model::*};
use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

pub struct OpenRouterImageClient {
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
        Ok(Self {
            config,
            http,
            endpoint,
            credentials,
            models,
        })
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
            let response = self
                .http
                .post(self.endpoint.clone())
                .header(reqwest::header::AUTHORIZATION, token.header())
                .json(&payload)
                .send()
                .await
                .map_err(|_| {
                    self.error(
                        ProviderErrorKind::Transport,
                        "image transport failed; completion is uncertain",
                    )
                })?;
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
