use aicoder_core::{
    model::{ModelToolCall, ToolDefinition},
    tools::*,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[derive(Default)]
struct Counts {
    prepared: AtomicUsize,
    executed: AtomicUsize,
    dropped: AtomicUsize,
    reviewed: AtomicUsize,
}
struct Tool {
    counts: Arc<Counts>,
    mode: u8,
}
struct Plan {
    counts: Arc<Counts>,
    large: bool,
}
impl Drop for Plan {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl PreparedTool for Plan {
    fn preview(&self) -> ToolPreview {
        ToolPreview {
            summary: "one immutable change".into(),
            body: if self.large {
                "x".repeat(MAX_TOOL_PREVIEW_BYTES)
            } else {
                "-old\n+new\n".into()
            },
            format: "unified_diff".into(),
            truncated: false,
        }
    }
    async fn execute(self: Box<Self>, _: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        self.counts.executed.fetch_add(1, Ordering::SeqCst);
        Ok(ToolSuccess::new(json!({"prepared":true})))
    }
}
#[async_trait]
impl ExecutableTool for Tool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "prepared".into(),
            description: None,
            parameters: json!({"type":"object"}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn prepare(
        &self,
        _: &ToolContext,
        _: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        self.counts.prepared.fetch_add(1, Ordering::SeqCst);
        if self.mode == 2 {
            return std::future::pending().await;
        }
        Ok(Some(Box::new(Plan {
            counts: self.counts.clone(),
            large: self.mode == 1,
        })))
    }
    async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
        panic!("dispatcher must consume the reviewed plan, never re-execute arguments")
    }
}
struct Review {
    counts: Arc<Counts>,
    allow: bool,
}
#[async_trait]
impl ApprovalHandler for Review {
    async fn approve(&self, invocation: &ToolInvocation) -> anyhow::Result<bool> {
        assert!(invocation.preview.as_ref().unwrap().body.contains("+new"));
        self.counts.reviewed.fetch_add(1, Ordering::SeqCst);
        Ok(self.allow)
    }
}
async fn run(mode: u8, allow: bool) -> (Value, Arc<Counts>) {
    let root = tempfile::tempdir().unwrap();
    let counts = Arc::new(Counts::default());
    let mut registry = ToolRegistry::default();
    registry
        .register(Tool {
            counts: counts.clone(),
            mode,
        })
        .unwrap();
    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        root.path(),
        Arc::new(Review {
            counts: counts.clone(),
            allow,
        }),
        DispatcherConfig {
            tool_timeout: Duration::from_millis(10),
            ..Default::default()
        },
    )
    .unwrap();
    let messages = dispatcher
        .dispatch(&[ModelToolCall {
            id: "call".into(),
            name: "prepared".into(),
            arguments: json!({}),
        }])
        .await
        .unwrap();
    (messages[0].tool_result().unwrap().1.clone(), counts)
}
#[tokio::test]
async fn approved_plans_execute_once_and_denied_plans_are_dropped() {
    for allow in [true, false] {
        let (result, counts) = run(0, allow).await;
        assert_eq!(result["ok"], allow);
        assert_eq!(counts.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(counts.reviewed.load(Ordering::SeqCst), 1);
        assert_eq!(counts.executed.load(Ordering::SeqCst), usize::from(allow));
        assert_eq!(counts.dropped.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn preparation_timeout_and_oversized_preview_never_reach_approval_or_execution() {
    for (mode, expected) in [(1, "preview_too_large"), (2, "timeout")] {
        let (result, counts) = run(mode, true).await;
        assert_eq!(result["error"]["code"], expected);
        assert_eq!(counts.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(counts.reviewed.load(Ordering::SeqCst), 0);
        assert_eq!(counts.executed.load(Ordering::SeqCst), 0);
    }
}
