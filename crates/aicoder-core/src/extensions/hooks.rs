use super::{name, size};
use crate::{InputContextProvider, PreparedInput, TurnExecutionContext, model::*, tools::*};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use futures::FutureExt;
use serde::Serialize;
use std::{future::Future, panic::AssertUnwindSafe, path::Path, sync::Arc, time::Duration};

/// Lower priority runs first; equal priorities retain extension/contribution installation order.
#[derive(Debug, Clone, Serialize)]
pub struct HookSpec {
    pub id: String,
    pub priority: i32,
    pub timeout: Duration,
}
impl HookSpec {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            priority: 0,
            timeout: Duration::from_secs(2),
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        name(&self.id)?;
        ensure!(
            (Duration::from_millis(1)..=Duration::from_secs(30)).contains(&self.timeout),
            "extension callback timeout must be 1 ms to 30 s"
        );
        Ok(())
    }
}
/// Cancellation observation only: no run controls, approval scope or ability to cancel the owner.
#[derive(Clone)]
pub struct ExtensionCancellation(TurnExecutionContext);
impl ExtensionCancellation {
    pub fn ui(&self) -> crate::ui::UiClient {
        self.0.ui()
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
    pub async fn cancelled(&self) {
        let _ = self.0.cancelled().await;
    }
}
struct Cancel(TurnExecutionContext);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel("extension callback ended");
    }
}
pub(crate) async fn invoke<T, F: Future<Output = Result<T>>>(
    owner: &str,
    spec: &HookSpec,
    ui: crate::ui::UiClient,
    action: impl FnOnce(ExtensionCancellation) -> F,
) -> Result<T> {
    let context = TurnExecutionContext::extension_ui(ui.named(owner));
    let _guard = Cancel(context.clone());
    let work =
        AssertUnwindSafe(async { action(ExtensionCancellation(context)).await }).catch_unwind();
    match tokio::time::timeout(spec.timeout, work).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(_))) => anyhow::bail!("extension {owner}/{} callback failed", spec.id),
        Ok(Err(_)) => anyhow::bail!("extension {owner}/{} callback panicked", spec.id),
        Err(_) => anyhow::bail!("extension {owner}/{} callback timed out", spec.id),
    }
}
pub struct ContextHookInput<'a> {
    pub workspace: &'a Path,
    pub messages: &'a [Message],
}
#[async_trait]
pub trait ContextHook: Send + Sync {
    /// Return additional system text for this turn. Recorded user/tool messages remain immutable.
    async fn context(
        &self,
        input: ContextHookInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> Result<Option<String>>;
}
pub struct ToolHookInput<'a> {
    pub workspace: &'a Path,
    pub call: &'a ModelToolCall,
    pub capability: ToolCapability,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolHookDecision {
    Continue,
    Deny { reason: String },
}
#[async_trait]
pub trait ToolHook: Send + Sync {
    /// Runs before read-only preparation and approval, including calls with cached grants.
    async fn before_tool(
        &self,
        input: ToolHookInput<'_>,
        cancellation: ExtensionCancellation,
    ) -> Result<ToolHookDecision>;
}
pub(crate) struct Registered<T: ?Sized> {
    pub owner: String,
    pub spec: HookSpec,
    pub callback: Arc<T>,
}
impl<T: ?Sized> Clone for Registered<T> {
    fn clone(&self) -> Self {
        Self {
            owner: self.owner.clone(),
            spec: self.spec.clone(),
            callback: self.callback.clone(),
        }
    }
}
pub(crate) struct ContextChain {
    pub base: Option<Arc<dyn InputContextProvider>>,
    pub hooks: Vec<Registered<dyn ContextHook>>,
    pub manifests: Vec<super::ExtensionManifest>,
}
#[async_trait]
impl InputContextProvider for ContextChain {
    async fn prepare(&self, workspace: &Path, messages: &[Message]) -> Result<PreparedInput> {
        self.prepare_with_prefix(workspace, messages, 0).await
    }
    async fn prepare_with_prefix(
        &self,
        workspace: &Path,
        messages: &[Message],
        system_prefix_len: usize,
    ) -> Result<PreparedInput> {
        self.prepare_for_execution(
            workspace,
            messages,
            system_prefix_len,
            &TurnExecutionContext::new(),
        )
        .await
    }
    async fn prepare_for_execution(
        &self,
        workspace: &Path,
        messages: &[Message],
        system_prefix_len: usize,
        context: &TurnExecutionContext,
    ) -> Result<PreparedInput> {
        ensure!(
            system_prefix_len <= messages.len(),
            "invalid runtime prefix boundary"
        );
        crate::input_context::validate_prefix(&messages[..system_prefix_len])?;
        let work = async {
            let mut prepared = match &self.base {
                Some(base) => {
                    base.prepare_for_execution(workspace, messages, system_prefix_len, context)
                        .await?
                }
                None => PreparedInput {
                    system_prefix: messages[..system_prefix_len].to_vec(),
                    provenance: serde_json::Value::Null,
                },
            };
            crate::input_context::validate_prefix(&prepared.system_prefix)?;
            let history = messages[system_prefix_len..].iter();
            let mut view = prepared.system_prefix.clone();
            view.extend(history.cloned());
            let mut prefix = prepared.system_prefix.len();
            let mut bytes = 0usize;
            let mut applied = Vec::new();
            for hook in &self.hooks {
                let text = invoke(&hook.owner, &hook.spec, context.ui(), |cancellation| {
                    hook.callback.context(
                        ContextHookInput {
                            workspace,
                            messages: &view,
                        },
                        cancellation,
                    )
                })
                .await?;
                if let Some(text) = text {
                    bytes = bytes
                        .checked_add(size(&text, 32 * 1024)?)
                        .ok_or_else(|| anyhow::anyhow!("context hook budget overflow"))?;
                    ensure!(
                        bytes <= 64 * 1024,
                        "context hooks exceed aggregate 64 KiB budget"
                    );
                    let message = Message::text(MessageRole::System, text);
                    view.insert(prefix, message.clone());
                    prefix += 1;
                    prepared.system_prefix.push(message);
                    applied.push(serde_json::json!({"extension":hook.owner,"hook":hook.spec.id}));
                }
            }
            prepared.provenance = serde_json::json!({"base":prepared.provenance,"extensions":self.manifests,"context_hooks":applied});
            Ok(prepared)
        };
        tokio::time::timeout(Duration::from_secs(30), work)
            .await
            .map_err(|_| anyhow::anyhow!("extension context phase timed out"))?
    }
}

pub(crate) async fn tool_hooks(
    hooks: &[Registered<dyn ToolHook>],
    workspace: &Path,
    call: &ModelToolCall,
    capability: ToolCapability,
    ui: &crate::ui::UiClient,
) -> Result<(), ToolFailure> {
    for hook in hooks {
        let decision = invoke(&hook.owner, &hook.spec, ui.clone(), |cancellation| {
            hook.callback.before_tool(
                ToolHookInput {
                    workspace,
                    call,
                    capability,
                },
                cancellation,
            )
        })
        .await
        .map_err(|e| ToolFailure::new("extension_hook_failed", e.to_string()))?;
        if let ToolHookDecision::Deny { reason } = decision {
            if size(&reason, 4096).is_err() {
                return Err(ToolFailure::new(
                    "extension_hook_failed",
                    "tool hook denial exceeds 4 KiB",
                ));
            }
            return Err(ToolFailure::new(
                "extension_denied",
                crate::redaction::sanitize_text(&reason, 4096),
            )
            .with_details(serde_json::json!({"extension":hook.owner,"hook":hook.spec.id})));
        }
    }
    Ok(())
}
