use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::tempdir;

use super::*;
use crate::{
    events::{AgentEventEmitter, AgentRawEvent, AgentRawEventEnvelope, ToolExecutionOutcome},
    types::{FunctionCall, FunctionDefinition, ToolCall, ToolType},
};

struct DenyApproval;

#[async_trait]
impl ApprovalHandler for DenyApproval {
    async fn approve(&self, _invocation: &ToolInvocation) -> Result<bool> {
        Ok(false)
    }
}

struct RecordingTool {
    name: &'static str,
    capability: ToolCapability,
    delay: Duration,
    events: Arc<Mutex<Vec<String>>>,
}

struct LargeOutputTool;

#[async_trait]
impl ExecutableTool for LargeOutputTool {
    fn definition(&self) -> FunctionDefinition {
        FunctionDefinition {
            name: "large_output".to_string(),
            description: None,
            parameters: Some(json!({"type": "object"})),
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
    fn definition(&self) -> FunctionDefinition {
        FunctionDefinition {
            name: self.name.to_string(),
            description: None,
            parameters: Some(json!({"type": "object"})),
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

fn tool_call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        tool_type: ToolType::Function,
        function: FunctionCall {
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

#[test]
fn registry_rejects_duplicate_names() {
    let mut registry = ToolRegistry::default();
    registry.register(ReadFileTool).unwrap();
    assert_eq!(registry.definitions().len(), 1);
    let error = registry.register(ReadFileTool).unwrap_err();
    assert!(error.to_string().contains("already registered"));
}

#[test]
fn default_registry_includes_edit_file_in_stable_order() {
    let names = default_registry()
        .unwrap()
        .definitions()
        .into_iter()
        .map(|tool| tool.function.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["read_file", "write_file", "edit_file", "bash", "grep"]
    );
}

#[tokio::test]
async fn read_and_write_file_stay_inside_workspace() {
    let directory = tempdir().unwrap();
    let context = ToolContext::new(directory.path(), Duration::from_secs(1), 1024).unwrap();
    WriteFileTool
        .execute(
            &context,
            json!({"path": "hello.txt", "content": "hello\nworld"}),
        )
        .await
        .unwrap();
    let result = ReadFileTool
        .execute(&context, json!({"path": "hello.txt", "limit_lines": 1}))
        .await
        .unwrap();
    assert_eq!(result.output["content"], "hello");

    let error = ReadFileTool
        .execute(&context, json!({"path": "../outside.txt"}))
        .await
        .unwrap_err();
    assert!(matches!(
        error.code.as_str(),
        "path_not_found" | "path_outside_workspace"
    ));
}

#[tokio::test]
async fn read_file_returns_bounded_pages() {
    let directory = tempdir().unwrap();
    let content = (1..=10)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(directory.path().join("large.txt"), content).unwrap();
    let context = ToolContext::new(directory.path(), Duration::from_secs(1), 4096).unwrap();

    let first_page = ReadFileTool
        .execute(
            &context,
            json!({"path": "large.txt", "offset_line": 3, "limit_lines": 2}),
        )
        .await
        .unwrap();
    assert_eq!(first_page.output["content"], "line-3\nline-4");
    assert_eq!(first_page.output["lines_read"], 2);
    assert_eq!(first_page.output["end_line"], 4);
    assert_eq!(first_page.output["has_more"], true);
    assert_eq!(first_page.output["next_offset_line"], 5);
    assert!(first_page.truncated);

    let last_page = ReadFileTool
        .execute(
            &context,
            json!({"path": "large.txt", "offset_line": 9, "limit_lines": 20}),
        )
        .await
        .unwrap();
    assert_eq!(last_page.output["content"], "line-9\nline-10");
    assert_eq!(last_page.output["lines_read"], 2);
    assert_eq!(last_page.output["has_more"], false);
    assert_eq!(last_page.output["next_offset_line"], Value::Null);
    assert!(!last_page.truncated);

    let past_eof = ReadFileTool
        .execute(
            &context,
            json!({"path": "large.txt", "offset_line": 99, "limit_lines": 2}),
        )
        .await
        .unwrap();
    assert_eq!(past_eof.output["lines_read"], 0);
    assert_eq!(past_eof.output["has_more"], false);
}

#[tokio::test]
async fn read_file_stops_at_output_byte_limit() {
    let directory = tempdir().unwrap();
    std::fs::write(directory.path().join("wide.txt"), "abcdefghij\nsecond").unwrap();
    let context = ToolContext::new(directory.path(), Duration::from_secs(1), 5).unwrap();

    let result = ReadFileTool
        .execute(
            &context,
            json!({"path": "wide.txt", "offset_line": 1, "limit_lines": 20}),
        )
        .await
        .unwrap();

    assert_eq!(result.output["content"], "abcde");
    assert_eq!(result.output["content_truncated"], true);
    assert_eq!(result.output["has_more"], true);
    assert!(result.truncated);
}

#[tokio::test]
async fn edit_file_applies_multiple_replacements_atomically() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("sample.rs");
    let original = "const TIMEOUT: u64 = 30;\nconst RETRIES: u32 = 1;\n";
    let expected = "const TIMEOUT: u64 = 60;\n";
    std::fs::write(&path, original).unwrap();
    let context = ToolContext::new(directory.path(), Duration::from_secs(1), 4096).unwrap();

    let result = EditFileTool
        .execute(
            &context,
            json!({
                "path": "sample.rs",
                "edits": [
                    {"old_text": "30", "new_text": "60"},
                    {"old_text": "const RETRIES: u32 = 1;\n", "new_text": ""}
                ]
            }),
        )
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    assert_eq!(result.output["edits_applied"], 2);
    assert_eq!(result.output["bytes_before"], original.len());
    assert_eq!(result.output["bytes_after"], expected.len());
}

#[tokio::test]
async fn edit_file_rejects_ambiguous_and_overlapping_matches() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("sample.txt");
    std::fs::write(&path, "aaa").unwrap();
    let context = ToolContext::new(directory.path(), Duration::from_secs(1), 4096).unwrap();

    let ambiguous = EditFileTool
        .execute(
            &context,
            json!({"path": "sample.txt", "edits": [{"old_text": "aa", "new_text": "x"}]}),
        )
        .await
        .unwrap_err();
    assert_eq!(ambiguous.code, "ambiguous_match");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "aaa");

    std::fs::write(&path, "abcdef").unwrap();
    let overlapping = EditFileTool
        .execute(
            &context,
            json!({
                "path": "sample.txt",
                "edits": [
                    {"old_text": "abc", "new_text": "x"},
                    {"old_text": "bc", "new_text": "y"}
                ]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(overlapping.code, "overlapping_edits");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "abcdef");
}

#[tokio::test]
async fn edit_file_does_not_write_when_any_match_is_missing() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("sample.txt");
    let original = "alpha beta gamma";
    std::fs::write(&path, original).unwrap();
    let context = ToolContext::new(directory.path(), Duration::from_secs(1), 4096).unwrap();

    let error = EditFileTool
        .execute(
            &context,
            json!({
                "path": "sample.txt",
                "edits": [
                    {"old_text": "alpha", "new_text": "ALPHA"},
                    {"old_text": "missing", "new_text": "value"}
                ]
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "match_not_found");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[cfg(unix)]
#[tokio::test]
async fn read_file_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("secret.txt");
    std::fs::write(&outside_file, "secret").unwrap();
    symlink(&outside_file, workspace.path().join("escape.txt")).unwrap();
    let context = ToolContext::new(workspace.path(), Duration::from_secs(1), 1024).unwrap();

    let error = ReadFileTool
        .execute(&context, json!({"path": "escape.txt"}))
        .await
        .unwrap_err();

    assert_eq!(error.code, "path_outside_workspace");
}

#[tokio::test]
async fn dispatcher_returns_approval_denied_as_tool_message() {
    let directory = tempdir().unwrap();
    let registry = Arc::new(default_registry().unwrap());
    let dispatcher = ToolDispatcher::new(
        registry,
        directory.path(),
        Arc::new(DenyApproval),
        DispatcherConfig::default(),
    )
    .unwrap();

    let delivered = Arc::new(Mutex::new(Vec::<AgentRawEventEnvelope>::new()));
    let captured = Arc::clone(&delivered);
    let events = AgentEventEmitter::new(Arc::new(move |event: &AgentRawEventEnvelope| {
        captured.lock().unwrap().push(event.clone());
    }));

    let messages = dispatcher
        .dispatch_with_events(
            &[tool_call(
                "call-1",
                "write_file",
                json!({"path": "denied.txt", "content": "no"}),
            )],
            &events,
        )
        .await
        .unwrap();
    events.shutdown().await;
    let result: Value = serde_json::from_str(messages[0].content.as_ref().unwrap()).unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "approval_denied");
    assert!(!directory.path().join("denied.txt").exists());

    let delivered = delivered.lock().unwrap();
    assert!(matches!(
        delivered[0].event,
        AgentRawEvent::ToolExecutionStarted { .. }
    ));
    assert!(matches!(
        delivered[1].event,
        AgentRawEvent::ToolApprovalRequested { .. }
    ));
    assert!(matches!(
        delivered[2].event,
        AgentRawEvent::ToolApprovalResolved {
            approved: false,
            ..
        }
    ));
    assert!(matches!(
        &delivered[3].event,
        AgentRawEvent::ToolExecutionEnded {
            outcome,
            ..
        } if matches!(outcome.as_ref(), ToolExecutionOutcome::ApprovalDenied)
    ));
}

#[tokio::test]
async fn bash_reports_non_zero_exit() {
    let directory = tempdir().unwrap();
    let context = ToolContext::new(directory.path(), Duration::from_secs(2), 1024).unwrap();
    let error = BashTool
        .execute(&context, json!({"command": "printf problem >&2; exit 7"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "command_failed");
    assert_eq!(error.details.unwrap()["exit_code"], 7);
}

#[tokio::test]
async fn dropping_bash_execution_kills_its_process_group() {
    let directory = tempdir().unwrap();
    let marker = directory.path().join("background-finished");
    let context = ToolContext::new(directory.path(), Duration::from_secs(5), 1024).unwrap();
    {
        let execution = BashTool.execute(
            &context,
            json!({"command": "(sleep 0.4; touch background-finished) & wait"}),
        );
        tokio::pin!(execution);

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            result = &mut execution => panic!("bash finished before cancellation: {result:?}"),
        }
    }
    tokio::time::sleep(Duration::from_millis(600)).await;

    assert!(!marker.exists(), "background process survived cancellation");
}

#[tokio::test]
async fn grep_finds_workspace_matches() {
    let directory = tempdir().unwrap();
    std::fs::write(directory.path().join("sample.rs"), "alpha\nbeta alpha\n").unwrap();
    let context = ToolContext::new(directory.path(), Duration::from_secs(2), 4096).unwrap();

    let result = GrepTool
        .execute(
            &context,
            json!({"pattern": "alpha", "glob": "*.rs", "max_results": 10}),
        )
        .await
        .unwrap();

    let matches = result.output["matches"].as_str().unwrap();
    assert!(matches.contains("sample.rs:1:alpha"));
    assert!(matches.contains("sample.rs:2:beta alpha"));
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

    assert_eq!(messages[0].tool_call_id.as_deref(), Some("one"));
    assert_eq!(messages[1].tool_call_id.as_deref(), Some("two"));
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("three"));
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
    let unknown: Value = serde_json::from_str(messages[0].content.as_ref().unwrap()).unwrap();
    let timed_out: Value = serde_json::from_str(messages[1].content.as_ref().unwrap()).unwrap();
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
    let result: Value = serde_json::from_str(messages[0].content.as_ref().unwrap()).unwrap();
    assert_eq!(result["truncated"], true);
    assert!(result["output"].as_str().unwrap().len() <= 32);

    let error = dispatcher
        .dispatch(&[call.clone(), call])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("limit exceeded"));
}
