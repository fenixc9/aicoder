//! Host-resolved tool images. Projections are ephemeral and never journal entries.
use crate::{
    TurnExecutionContext,
    artifacts::ArtifactRef,
    model::{ContentBlock, ImageSource, Message, MessageRole, ModelSpec},
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{borrow::Cow, time::Duration};

pub const MAX_TOOL_IMAGES: usize = 4;
/// Four previews together admit at most one MiB of decoded normalized bytes.
pub const MAX_TOOL_IMAGE_BYTES: usize = 256 * 1024;
pub const MAX_TOOL_IMAGE_SOURCE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolImageError {
    Unavailable,
    Invalid,
    Limit,
    Timeout,
}
impl ToolImageError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Invalid => "invalid_image",
            Self::Limit => "image_limit",
            Self::Timeout => "image_timeout",
        }
    }
}
impl std::fmt::Display for ToolImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ToolImageError {}

/// A host-validated static preview, not a claim that the original image was lossless.
#[derive(Debug)]
pub struct ResolvedToolImage(ImageSource);
impl ResolvedToolImage {
    /// The resolver must validate the actual codec, dimensions and immutable source bytes.
    /// Core additionally bounds and validates the inline envelope without codec dependencies.
    pub fn new(media_type: String, data: String) -> Result<Self, ToolImageError> {
        let body = data.trim_end_matches('=');
        let padding = data.len() - body.len();
        if !matches!(
            media_type.as_str(),
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ) || data.is_empty()
            || !data.len().is_multiple_of(4)
            || padding > 2
            || data.len() > MAX_TOOL_IMAGE_BYTES.div_ceil(3) * 4
            || data.len() / 4 * 3 - padding > MAX_TOOL_IMAGE_BYTES
            || !body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
        {
            return Err(ToolImageError::Limit);
        }
        Ok(Self(ImageSource::Base64 { media_type, data }))
    }
    pub fn source(&self) -> &ImageSource {
        &self.0
    }
}

/// Explicit scoped host authority; references/history cannot install a resolver or store.
/// Resolution must be read-only, cancellation-aware and bounded. No implicit URL fetches.
#[async_trait]
pub trait ToolImageResolver: Send + Sync {
    async fn resolve(
        &self,
        reference: &ArtifactRef,
        context: TurnExecutionContext,
    ) -> Result<ResolvedToolImage, ToolImageError>;
}

struct CancelResolution(TurnExecutionContext);
impl Drop for CancelResolution {
    fn drop(&mut self) {
        self.0.cancel("tool image resolution finished or dropped");
    }
}

const NOTICE: &str = "_tool_image_projection";
fn notice(message: &mut Message, reference: Option<&ArtifactRef>, status: &str) {
    if let Some(ContentBlock::ToolResult { content, .. }) = message
        .content
        .iter_mut()
        .find(|block| matches!(block, ContentBlock::ToolResult { .. }))
    {
        if !content.is_object() {
            *content = json!({"tool_result":content.take()});
        }
        // This reserved field exists only on a model-view clone, never canonical output.
        let entries = content
            .as_object_mut()
            .unwrap()
            .entry(NOTICE)
            .or_insert_with(|| json!([]));
        if let Some(entries) = entries.as_array_mut()
            && entries.len() < 8
        {
            let mut entry = json!({"status":status});
            if let Some(reference) = reference {
                entry["artifact_id"] = json!(reference.id);
            }
            entries.push(entry);
        }
    }
}

/// Shared pure adaptation for context decisions, pruning, navigation and request preparation.
/// Select the newest four tool images. User images retain their strict capability checks.
pub(crate) fn model_messages<'a>(messages: &'a [Message], spec: &ModelSpec) -> Cow<'a, [Message]> {
    let changes = messages.iter().any(|message| {
        (message.role == MessageRole::Assistant
            && !spec.capabilities.reasoning_history
            && message
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Reasoning { .. })))
            || (message.role == MessageRole::Tool
                && message
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Image { .. })))
    });
    if !changes {
        return Cow::Borrowed(messages);
    }
    let mut remaining = MAX_TOOL_IMAGES;
    let mut projected = Vec::with_capacity(messages.len());
    for original in messages.iter().rev() {
        let mut message = original.clone();
        if message.role == MessageRole::Assistant && !spec.capabilities.reasoning_history {
            message
                .content
                .retain(|b| !matches!(b, ContentBlock::Reasoning { .. }));
            if message.content.is_empty() {
                continue;
            }
        }
        if message.role == MessageRole::Tool
            && message
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Image { .. }))
        {
            // Discard any tool-controlled field before writing host projection observations.
            for block in &mut message.content {
                if let ContentBlock::ToolResult {
                    content: Value::Object(content),
                    ..
                } = block
                {
                    content.remove(NOTICE);
                }
            }
            let mut kept = Vec::with_capacity(message.content.len());
            let mut notices = Vec::new();
            for block in std::mem::take(&mut message.content).into_iter().rev() {
                if let ContentBlock::Image { source } = &block {
                    let status = match source {
                        _ if !spec.capabilities.images => Some("model_without_vision"),
                        ImageSource::Artifact { reference } if !reference.complete => {
                            Some("incomplete_source")
                        }
                        ImageSource::Artifact { reference }
                            if reference.bytes > MAX_TOOL_IMAGE_SOURCE_BYTES =>
                        {
                            Some("image_limit")
                        }
                        _ if remaining == 0 => Some("image_limit"),
                        _ => None,
                    };
                    if let Some(status) = status {
                        let reference = match source {
                            ImageSource::Artifact { reference } => Some(reference.clone()),
                            _ => None,
                        };
                        if notices.len() < 8 {
                            notices.push((reference, status));
                        }
                    } else {
                        remaining -= 1;
                        kept.push(block);
                    }
                } else {
                    kept.push(block);
                }
            }
            kept.reverse();
            message.content = kept;
            for (reference, status) in notices {
                notice(&mut message, reference.as_ref(), status);
            }
        }
        projected.push(message);
    }
    projected.reverse();
    Cow::Owned(projected)
}

/// Resolve only the selected view. Failure omits a preview, preserves tool completion and
/// cannot cause another tool execution. Cancellation aborts before a model request starts.
pub(crate) async fn materialize(
    messages: &mut [Message],
    resolver: Option<&dyn ToolImageResolver>,
    context: &TurnExecutionContext,
) -> Result<(), crate::TurnCancelled> {
    for message in messages.iter_mut().filter(|m| m.role == MessageRole::Tool) {
        let mut index = 0;
        let mut failures = Vec::new();
        let mut observations = Vec::new();
        for block in &mut message.content {
            let ContentBlock::Image { source } = block else {
                continue;
            };
            if context.is_cancelled() {
                return Err(context.error());
            }
            let reference = match source {
                ImageSource::Artifact { reference } => Some(reference.clone()),
                _ => None,
            };
            let child = context.child();
            let _cancel = CancelResolution(child.clone());
            let result = match source {
                ImageSource::Artifact { reference } => match resolver {
                    Some(resolver) => tokio::select! {
                        biased;
                        cancelled = context.cancelled() => { child.cancel("image projection cancelled"); return Err(cancelled); },
                        result = tokio::time::timeout(Duration::from_secs(2), resolver.resolve(reference, child.clone())) => result.unwrap_or(Err(ToolImageError::Timeout)),
                    },
                    None => Err(ToolImageError::Unavailable),
                },
                ImageSource::Base64 { media_type, data } => {
                    ResolvedToolImage::new(media_type.clone(), data.clone())
                }
                ImageSource::Url { .. } => Err(ToolImageError::Invalid),
            };
            child.cancel("image projection finished");
            match result {
                Ok(preview) => {
                    *source = preview.0;
                    observations.push((reference, "normalized_preview"));
                }
                Err(error) => {
                    failures.push(index);
                    observations.push((reference, error.code()));
                }
            }
            index += 1;
        }
        let mut current = 0;
        message.content.retain(|block| {
            if matches!(block, ContentBlock::Image { .. }) {
                let keep = !failures.contains(&current);
                current += 1;
                keep
            } else {
                true
            }
        });
        for (reference, code) in observations {
            notice(message, reference.as_ref(), code);
        }
    }
    if context.is_cancelled() {
        return Err(context.error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentInput,
        model::*,
        tokens::{TokenCounting, TokenFallback},
    };
    use std::sync::{Arc, Mutex};
    fn reference() -> ArtifactRef {
        ArtifactRef {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            media_type: "image/png".into(),
            bytes: 3,
            complete: true,
        }
    }
    fn history(count: usize) -> Vec<Message> {
        let mut messages = vec![Message::text(MessageRole::User, "inspect")];
        for i in 0..count {
            messages.push(Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: i.to_string(),
                        name: "capture".into(),
                        arguments: json!({}),
                    },
                }],
            });
            messages.push(Message {
                role: MessageRole::Tool,
                content: vec![
                    ContentBlock::ToolResult {
                        call_id: i.to_string(),
                        content: json!({"ok":true}),
                        is_error: false,
                    },
                    ContentBlock::Image {
                        source: ImageSource::Artifact {
                            reference: reference(),
                        },
                    },
                ],
            });
        }
        messages
    }
    fn spec(vision: bool) -> ModelSpec {
        let mut spec = ModelSpec::new("fixture", "vision");
        spec.capabilities.tools = true;
        spec.capabilities.images = vision;
        spec
    }
    fn image_count(messages: &[Message]) -> usize {
        messages
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| matches!(b, ContentBlock::Image { .. }))
            .count()
    }
    #[test]
    fn projections_select_newest_four_and_never_mutate_canonical_facts() {
        let messages = history(6);
        let original = messages.clone();
        let projected = model_messages(&messages, &spec(true));
        assert_eq!(image_count(&projected), 4);
        for i in 0..2 {
            assert_eq!(
                projected[2 + i * 2].tool_result().unwrap().1[NOTICE][0]["status"],
                "image_limit"
            );
        }
        ModelRequest::new("vision", projected.into_owned())
            .validate(&spec(true))
            .unwrap();
        let projected = model_messages(&messages, &spec(false));
        assert_eq!(image_count(&projected), 0);
        assert_eq!(
            projected[2].tool_result().unwrap().1[NOTICE][0]["status"],
            "model_without_vision"
        );
        ModelRequest::new("vision", projected.into_owned())
            .validate(&spec(false))
            .unwrap();
        assert_eq!(messages, original);
        let user_artifact = Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Image {
                source: ImageSource::Artifact {
                    reference: reference(),
                },
            }],
        };
        assert!(AgentInput::try_from(user_artifact.clone()).is_err());
        assert!(
            ModelRequest::new("vision", vec![user_artifact])
                .validate(&spec(true))
                .is_err()
        );
        let mut invalid = original[2].clone();
        invalid.role = MessageRole::User;
        assert!(AgentInput::try_from(invalid).is_err());
        let mut request = ModelRequest::new("vision", original);
        request.messages[2].content.push(ContentBlock::ToolResult {
            call_id: "extra".into(),
            content: json!({}),
            is_error: false,
        });
        assert!(request.validate(&spec(true)).is_err());
    }
    struct SelectionProvider;
    #[async_trait]
    impl ModelProvider for SelectionProvider {
        fn model_spec(&self, _: &str) -> Result<ModelSpec, ProviderError> {
            Ok(spec(false))
        }
        async fn generate(
            &self,
            _: ModelRequest,
            _: Arc<dyn ModelEventSink>,
            _: TurnExecutionContext,
        ) -> Result<ModelResponse, ProviderError> {
            panic!("selection must not invoke provider")
        }
    }
    #[test]
    fn selection_uses_the_same_nonvision_projection_without_loading_images() {
        let root = tempfile::tempdir().unwrap();
        let executor = crate::TurnExecutor::builder(SelectionProvider)
            .workspace(root.path())
            .build()
            .unwrap();
        let canonical = history(1);
        let input =
            crate::ExecutionInput::from_request(ModelRequest::new("vision", canonical.clone()));
        let report = executor.validate_model_selection(&input).unwrap();
        assert!(!report.needs_compaction);
        assert_eq!(input.request.messages, canonical);
        assert_eq!(
            report.input_token_estimate,
            TokenCounting::default()
                .request(
                    &ModelRequest::new(
                        "vision",
                        model_messages(&canonical, &spec(false)).into_owned()
                    ),
                    &spec(false)
                )
                .tokens
        );
    }
    struct Resolver;
    #[async_trait]
    impl ToolImageResolver for Resolver {
        async fn resolve(
            &self,
            _: &ArtifactRef,
            _: TurnExecutionContext,
        ) -> Result<ResolvedToolImage, ToolImageError> {
            ResolvedToolImage::new("image/jpeg".into(), "AQID".into())
        }
    }
    #[tokio::test]
    async fn materialization_is_bounded_estimated_and_reference_only_failures_are_explicit() {
        let original = history(1);
        let mut view = model_messages(&original, &spec(true)).into_owned();
        materialize(&mut view, Some(&Resolver), &TurnExecutionContext::new())
            .await
            .unwrap();
        assert!(matches!(
            &view[2].content[1],
            ContentBlock::Image {
                source: ImageSource::Base64 { .. }
            }
        ));
        assert_eq!(
            view[2].tool_result().unwrap().1[NOTICE][0]["status"],
            "normalized_preview"
        );
        let counting = TokenCounting::default();
        let count = counting.request(&ModelRequest::new("vision", view.clone()), &spec(true));
        if let ContentBlock::Image { source } = &mut view[2].content[1] {
            *source = ImageSource::Base64 {
                media_type: "image/jpeg".into(),
                data: "A".repeat(300_000),
            };
        }
        assert_eq!(
            count,
            counting.request(&ModelRequest::new("vision", view), &spec(true))
        );
        assert_eq!(count.fallback, Some(TokenFallback::UnsupportedContent));
        assert!(
            ResolvedToolImage::new(
                "image/png".into(),
                "A".repeat(MAX_TOOL_IMAGE_BYTES.div_ceil(3) * 4)
            )
            .is_err()
        );
        let mut view = model_messages(&original, &spec(true)).into_owned();
        materialize(&mut view, None, &TurnExecutionContext::new())
            .await
            .unwrap();
        assert_eq!(image_count(&view), 0);
        assert_eq!(
            view[2].tool_result().unwrap().1[NOTICE][0]["status"],
            "unavailable"
        );
        assert_eq!(image_count(&original), 1);
    }
    struct Pending(
        Arc<tokio::sync::Notify>,
        Arc<Mutex<Option<TurnExecutionContext>>>,
    );
    #[async_trait]
    impl ToolImageResolver for Pending {
        async fn resolve(
            &self,
            _: &ArtifactRef,
            context: TurnExecutionContext,
        ) -> Result<ResolvedToolImage, ToolImageError> {
            *self.1.lock().unwrap() = Some(context);
            self.0.notify_one();
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn cancellation_and_dropped_projection_cancel_owned_resolver_children() {
        for drop_future in [false, true] {
            let started = Arc::new(tokio::sync::Notify::new());
            let retained = Arc::new(Mutex::new(None));
            let resolver = Pending(started.clone(), retained.clone());
            let context = TurnExecutionContext::new();
            let worker = context.clone();
            let task = tokio::spawn(async move {
                let mut view = history(1);
                materialize(&mut view, Some(&resolver), &worker).await
            });
            started.notified().await;
            if drop_future {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                context.cancel("stop projection");
                assert!(task.await.unwrap().is_err());
            }
            assert!(retained.lock().unwrap().as_ref().unwrap().is_cancelled());
        }
    }
    #[tokio::test]
    async fn timeout_omits_preview_and_expires_the_resolver_without_cancelling_the_turn() {
        let retained = Arc::new(Mutex::new(None));
        let resolver = Pending(Arc::new(tokio::sync::Notify::new()), retained.clone());
        let context = TurnExecutionContext::new();
        let mut view = history(1);
        materialize(&mut view, Some(&resolver), &context)
            .await
            .unwrap();
        assert_eq!(image_count(&view), 0);
        assert_eq!(
            view[2].tool_result().unwrap().1[NOTICE][0]["status"],
            "image_timeout"
        );
        assert!(retained.lock().unwrap().as_ref().unwrap().is_cancelled());
        assert!(!context.is_cancelled());
    }
}
