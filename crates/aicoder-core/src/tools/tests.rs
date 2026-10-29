use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::tempdir;

use super::*;
use crate::model::{ModelToolCall, ToolDefinition};

struct RecordingTool {
    name: &'static str,
    capability: ToolCapability,
    delay: Duration,
    events: Arc<Mutex<Vec<String>>>,
}

struct LargeOutputTool;

#[async_trait]
impl ExecutableTool for LargeOutputTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "large_output".to_string(),
            description: None,
            parameters: json!({"type": "object"}),
        }
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }

    async fn execute(
        &self,
        _context: &ToolContext,
        _arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        Ok(ToolSuccess::new(Value::String("x".repeat(1024))))
    }
}

#[async_trait]
impl ExecutableTool for RecordingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.to_string(),
            description: None,
            parameters: json!({"type": "object"}),
        }
    }

    fn capability(&self) -> ToolCapability {
        self.capability
    }

    async fn execute(
        &self,
        _context: &ToolContext,
        _arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        self.events
            .lock()
            .unwrap()
            .push(format!("start:{}", self.name));
        tokio::time::sleep(self.delay).await;
        self.events
            .lock()
            .unwrap()
            .push(format!("end:{}", self.name));
        Ok(ToolSuccess::new(json!({"tool": self.name})))
    }
}

fn tool_call(id: &str, name: &str, arguments: Value) -> ModelToolCall {
    ModelToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    }
}

#[test]
fn registry_rejects_duplicate_names() {
    let mut registry = ToolRegistry::default();
    registry.register(LargeOutputTool).unwrap();
    assert_eq!(registry.definitions().len(), 1);
    let error = registry.register(LargeOutputTool).unwrap_err();
    assert!(error.to_string().contains("already registered"));
}

#[tokio::test]
async fn dispatcher_preserves_order_and_places_write_barrier() {
    let directory = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::default();
    registry
        .register(RecordingTool {
            name: "read_slow",
            capability: ToolCapability::ReadOnly,
            delay: Duration::from_millis(30),
            events: events.clone(),
        })
        .unwrap();
    registry
        .register(RecordingTool {
            name: "read_fast",
            capability: ToolCapability::ReadOnly,
            delay: Duration::from_millis(5),
            events: events.clone(),
        })
        .unwrap();
    registry
        .register(RecordingTool {
            name: "write",
            capability: ToolCapability::Mutating,
            delay: Duration::ZERO,
            events: events.clone(),
        })
        .unwrap();
    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        directory.path(),
        Arc::new(AllowAllApproval),
        DispatcherConfig::default(),
    )
    .unwrap();
    let calls = vec![
        tool_call("one", "read_slow", json!({})),
        tool_call("two", "read_fast", json!({})),
        tool_call("three", "write", json!({})),
    ];

    let messages = dispatcher.dispatch(&calls).await.unwrap();

    assert_eq!(messages[0].tool_result().map(|(id, _, _)| id), Some("one"));
    assert_eq!(messages[1].tool_result().map(|(id, _, _)| id), Some("two"));
    assert_eq!(
        messages[2].tool_result().map(|(id, _, _)| id),
        Some("three")
    );
    let events = events.lock().unwrap();
    let write_start = events
        .iter()
        .position(|event| event == "start:write")
        .unwrap();
    let slow_end = events
        .iter()
        .position(|event| event == "end:read_slow")
        .unwrap();
    let fast_end = events
        .iter()
        .position(|event| event == "end:read_fast")
        .unwrap();
    assert!(write_start > slow_end);
    assert!(write_start > fast_end);
}

#[tokio::test]
async fn dispatcher_reports_unknown_tool_and_timeout() {
    let directory = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::default();
    registry
        .register(RecordingTool {
            name: "slow",
            capability: ToolCapability::ReadOnly,
            delay: Duration::from_millis(100),
            events,
        })
        .unwrap();
    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        directory.path(),
        Arc::new(AllowAllApproval),
        DispatcherConfig {
            tool_timeout: Duration::from_millis(5),
            ..DispatcherConfig::default()
        },
    )
    .unwrap();

    let messages = dispatcher
        .dispatch(&[
            tool_call("unknown", "missing", json!({})),
            tool_call("timeout", "slow", json!({})),
        ])
        .await
        .unwrap();
    let unknown: Value = messages[0].tool_result().unwrap().1.clone();
    let timed_out: Value = messages[1].tool_result().unwrap().1.clone();
    assert_eq!(unknown["error"]["code"], "unknown_tool");
    assert_eq!(timed_out["error"]["code"], "timeout");
}

#[tokio::test]
async fn dispatcher_enforces_call_and_output_limits() {
    let directory = tempdir().unwrap();
    let mut registry = ToolRegistry::default();
    registry.register(LargeOutputTool).unwrap();
    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        directory.path(),
        Arc::new(AllowAllApproval),
        DispatcherConfig {
            max_calls_per_round: 1,
            max_output_bytes: 32,
            ..DispatcherConfig::default()
        },
    )
    .unwrap();
    let call = tool_call("large", "large_output", json!({}));

    let messages = dispatcher
        .dispatch(std::slice::from_ref(&call))
        .await
        .unwrap();
    let result: Value = messages[0].tool_result().unwrap().1.clone();
    assert_eq!(result["truncated"], true);
    assert!(result["output"].as_str().unwrap().len() <= 32);

    let error = dispatcher
        .dispatch(&[call.clone(), call])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("limit exceeded"));
}
