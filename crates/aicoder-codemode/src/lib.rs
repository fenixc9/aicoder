//! Optional QuickJS tool programs. The core dispatcher retains all external IO authority.
#![cfg(feature = "engine")]
mod engine;
use aicoder_core::{composition::*, model::ToolDefinition, tools::*};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};
pub const IMAGE_OUTPUT_TOOL: &str = "image_output";
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CodeModeConfig {
    /// Explicit host registration used by image(); the program allowlist still applies.
    pub image_tool: Option<String>,
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub interrupt_checks: usize,
    pub max_jobs: usize,
    pub max_output_bytes: usize,
    pub max_output_items: usize,
    pub max_workers: usize,
    pub limits: CompositionLimits,
}
impl Default for CodeModeConfig {
    fn default() -> Self {
        Self {
            image_tool: Some(IMAGE_OUTPUT_TOOL.into()),
            memory_bytes: 8 * 1024 * 1024,
            stack_bytes: 256 * 1024,
            interrupt_checks: 100000,
            max_jobs: 20000,
            max_output_bytes: 32768,
            max_output_items: 128,
            max_workers: 4,
            limits: CompositionLimits::default(),
        }
    }
}
impl CodeModeConfig {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.image_tool {
            ensure!(
                !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control),
                "invalid image output tool identity"
            );
        }
        ensure!(
            (1024 * 1024..=64 * 1024 * 1024).contains(&self.memory_bytes)
                && (64 * 1024..=1024 * 1024).contains(&self.stack_bytes)
                && (1..=1000000).contains(&self.interrupt_checks)
                && (1..=100000).contains(&self.max_jobs)
                && (512..=256 * 1024).contains(&self.max_output_bytes)
                && (1..=1024).contains(&self.max_output_items)
                && (1..=16).contains(&self.max_workers),
            "invalid Code Mode VM limits"
        );
        ensure!(
            (1..=64).contains(&self.limits.max_calls)
                && (1..=8).contains(&self.limits.max_parallel)
                && (512..=64 * 1024).contains(&self.limits.max_request_bytes)
                && (512..=256 * 1024).contains(&self.limits.max_result_bytes)
                && (1024..=256 * 1024).contains(&self.limits.max_state_bytes)
                && (Duration::from_millis(1)..=Duration::from_secs(600))
                    .contains(&self.limits.duration),
            "invalid Code Mode composition limits"
        );
        Ok(())
    }
}
pub struct CodeModeTool {
    config: CodeModeConfig,
    allowed: Option<Vec<String>>,
    active: Arc<AtomicUsize>,
}
impl CodeModeTool {
    pub fn new(config: CodeModeConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            allowed: None,
            active: Arc::new(AtomicUsize::new(0)),
        })
    }
    /// Host selection only. None is all non-composite entries in the frozen registry.
    pub fn only_tools(mut self, tools: Vec<String>) -> Result<Self> {
        ensure!(
            tools.len() <= 256
                && tools
                    .iter()
                    .all(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)),
            "invalid Code Mode tool selection"
        );
        let mut seen = std::collections::BTreeSet::new();
        ensure!(
            tools.iter().all(|name| seen.insert(name)),
            "duplicate Code Mode tool selection"
        );
        self.allowed = Some(tools);
        Ok(self)
    }
    pub fn active_workers(&self) -> usize {
        self.active.load(std::sync::atomic::Ordering::Acquire)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    code: String,
}
impl ToolCompositionFactory for CodeModeTool {
    fn allowed_tools(&self) -> Option<&[String]> {
        self.allowed.as_deref()
    }
    fn limits(&self) -> CompositionLimits {
        self.config.limits
    }
    fn state_namespace(&self) -> Option<&str> {
        Some("aicoder.codemode.store.v1")
    }
    fn start(
        &self,
        context: CompositionContext,
        arguments: Value,
    ) -> Result<Box<dyn ToolProgram>, ToolFailure> {
        let args: Arguments = serde_json::from_value(arguments).map_err(|_| {
            ToolFailure::new(
                "code_mode_arguments",
                "Expected only a JavaScript code field",
            )
        })?;
        if args.code.trim().is_empty() || args.code.len() > 64 * 1024 {
            return Err(ToolFailure::new(
                "code_mode_arguments",
                "JavaScript source must contain 1..64 KiB",
            ));
        }
        engine::start(args.code, context, self.config.clone(), self.active.clone())
    }
}
#[async_trait]
impl ExecutableTool for CodeModeTool {
    fn composition_factory(&self) -> Option<&dyn ToolCompositionFactory> {
        Some(self)
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {name:"codemode".into(),description:Some("Run bounded JavaScript as an async function body. Use await tools.NAME(args), Promise.allSettled, loops and JSON filtering. text(value), await image(source), models.getModelsOfType/getAvailableOfType/getModelOfType, models.classify, console.log, return, exit, ALL_TOOLS, searchTools, describeTool, describeNamespace, store/load are available. Use tools.NAME.result(args) for the host envelope/artifacts; await image(envelope) selects its images. image also accepts {artifact_id} or {data,mimeType}. Only explicit output reaches the next model request. Tool permission and budgets remain separate; unawaited calls are cancelled when the script ends. No filesystem, network, Node, timers, imports or credential access is provided. Result: program {status,items,console} plus core composition metadata; failures retain bounded partial output.".into()),parameters:json!({"type":"object","properties":{"code":{"type":"string","maxLength":65536}},"required":["code"],"additionalProperties":false})}
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Command
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        Err(ToolFailure::new(
            "composition_scope_required",
            "Code Mode requires the scoped core dispatcher",
        ))
    }
}
