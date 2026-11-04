//! Bounded native image-generation requests and immutable host delivery plans.
use super::*;

pub const MAX_GENERATED_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_GENERATED_TOTAL_BYTES: usize = 16 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageGenerationModel {
    #[serde(flatten)]
    pub specification: AuxiliaryModel,
    #[serde(default)]
    pub text_output: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageGenerationInput {
    Text { text: String },
    Image { source: ImageSource },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageGenerationContext {
    pub input: Vec<ImageGenerationInput>,
}
impl ImageGenerationContext {
    pub fn validate(&self, model: &ImageGenerationModel) -> anyhow::Result<()> {
        model.specification.validate()?;
        anyhow::ensure!(
            (1..=32).contains(&self.input.len()),
            "invalid image input count"
        );
        let mut images = 0;
        let mut text_bytes = 0usize;
        for block in &self.input {
            match block {
                ImageGenerationInput::Text { text } => {
                    anyhow::ensure!(!text.is_empty(), "empty image prompt block");
                    text_bytes = text_bytes.saturating_add(text.len());
                    anyhow::ensure!(text_bytes <= 64 * 1024, "image prompt limit exceeded");
                }
                ImageGenerationInput::Image { source } => {
                    images += 1;
                    anyhow::ensure!(
                        model.specification.images && images <= 4,
                        "image input unsupported or excessive"
                    );
                    validate_image_source(source)?;
                }
            }
        }
        Ok(())
    }
    pub fn input_count(
        &self,
        counting: &TokenCounting,
        model: &ImageGenerationModel,
    ) -> TokenCount {
        let mut tokens = 64u64;
        for block in &self.input {
            tokens = tokens.saturating_add(match block {
                ImageGenerationInput::Text { text } => {
                    counting
                        .count(TokenInput {
                            provider: &model.specification.provider,
                            model: &model.specification.id,
                            tokenizer: None,
                            content: TokenContent::Text(text),
                        })
                        .tokens
                }
                ImageGenerationInput::Image { .. } => 8192,
            });
        }
        let mut count = TokenCount::estimated(tokens, TokenScope::Request, "auxiliary_images_v1");
        count.fallback = Some(TokenFallback::UnsupportedContent);
        count
    }
}
/// Ephemeral provider output. Raw bytes are delivered to explicit host storage, never history.
#[derive(Debug)]
pub enum GeneratedBlock {
    Text { text: String },
    Image { media_type: String, bytes: Vec<u8> },
}
pub fn validate_generated_output(
    output: &[GeneratedBlock],
    model: &ImageGenerationModel,
) -> anyhow::Result<()> {
    anyhow::ensure!(output.len() <= 16, "too many generated blocks");
    let mut images = 0;
    let mut total = 0usize;
    let mut text_bytes = 0usize;
    for block in output {
        match block {
            GeneratedBlock::Text { text } => {
                text_bytes = text_bytes.saturating_add(text.len());
                anyhow::ensure!(
                    model.text_output && text_bytes <= 64 * 1024,
                    "unexpected or oversized generated text"
                );
            }
            GeneratedBlock::Image { media_type, bytes } => {
                images += 1;
                total = total.saturating_add(bytes.len());
                let mime = crate::media::image_media_type(media_type);
                anyhow::ensure!(
                    images <= 4
                        && !bytes.is_empty()
                        && bytes.len() <= MAX_GENERATED_IMAGE_BYTES
                        && total <= MAX_GENERATED_TOTAL_BYTES
                        && mime.as_deref().is_some_and(|m| matches!(
                            m,
                            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                        )),
                    "invalid generated image envelope"
                );
            }
        }
    }
    anyhow::ensure!(images > 0, "no image was generated");
    Ok(())
}
#[async_trait]
pub trait ImageGenerationProvider: Send + Sync {
    async fn generate_images(
        &self,
        model: &ImageGenerationModel,
        context: &ImageGenerationContext,
        usage: AuxiliaryUsageReporter,
        cancellation: TurnExecutionContext,
    ) -> Result<Vec<GeneratedBlock>, ProviderError>;
}
#[async_trait]
pub trait ImageGenerationOperation: Send + Sync {
    fn model(&self) -> &ImageGenerationModel;
    fn context(&self) -> &ImageGenerationContext;
    fn provider(&self) -> &dyn ImageGenerationProvider;
    async fn deliver(
        &self,
        output: Vec<GeneratedBlock>,
        usage: Option<ModelUsage>,
        context: &ToolContext,
    ) -> Result<ToolSuccess, ToolFailure>;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model() -> ImageGenerationModel {
        ImageGenerationModel {
            specification: AuxiliaryModel {
                provider: "painter".into(),
                id: "native".into(),
                name: String::new(),
                images: true,
                context_tokens: 65536,
                output_reservation: 1024,
            },
            text_output: false,
        }
    }
    #[test]
    fn image_input_bounds_capabilities_and_counting_exclude_encoded_payload() {
        let mut context = ImageGenerationContext {
            input: vec![ImageGenerationInput::Image {
                source: ImageSource::Base64 {
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                },
            }],
        };
        context.validate(&model()).unwrap();
        let first = context.input_count(&TokenCounting::default(), &model());
        if let ImageGenerationInput::Image {
            source: ImageSource::Base64 { data, .. },
        } = &mut context.input[0]
        {
            *data = "AAAA".repeat(1024);
        }
        context.validate(&model()).unwrap();
        let second = context.input_count(&TokenCounting::default(), &model());
        assert_eq!(first.tokens, second.tokens);
        assert_eq!(first.tokens, 8256);
        assert_eq!(first.accuracy, TokenAccuracy::Estimated);
        let mut text_only = model();
        text_only.specification.images = false;
        assert!(context.validate(&text_only).is_err());
        context.input = vec![ImageGenerationInput::Image {
            source: ImageSource::Url {
                url: "https://must-not-fetch.invalid".into(),
            },
        }];
        assert!(context.validate(&model()).is_err());
        context.input = vec![ImageGenerationInput::Text {
            text: "x".repeat(65537),
        }];
        assert!(context.validate(&model()).is_err());
        context.input.clear();
        assert!(context.validate(&model()).is_err());
    }
    #[test]
    fn generated_output_requires_images_and_enforces_raw_delivery_bounds() {
        let image = || GeneratedBlock::Image {
            media_type: "image/png".into(),
            bytes: vec![1],
        };
        assert!(validate_generated_output(&[], &model()).is_err());
        assert!(
            validate_generated_output(
                &[GeneratedBlock::Text {
                    text: "no image".into()
                }],
                &model()
            )
            .is_err()
        );
        assert!(
            validate_generated_output(
                &[GeneratedBlock::Image {
                    media_type: "text/plain".into(),
                    bytes: vec![1]
                }],
                &model()
            )
            .is_err()
        );
        assert!(
            validate_generated_output(&(0..5).map(|_| image()).collect::<Vec<_>>(), &model())
                .is_err()
        );
        assert!(
            validate_generated_output(
                &[GeneratedBlock::Image {
                    media_type: "image/png".into(),
                    bytes: vec![0; MAX_GENERATED_IMAGE_BYTES + 1]
                }],
                &model()
            )
            .is_err()
        );
        validate_generated_output(&[image()], &model()).unwrap();
    }
}
