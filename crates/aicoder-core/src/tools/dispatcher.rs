use std::{path::Path, sync::Arc, time::Duration};

use anyhow::Result;
use futures::future::join_all;
use serde_json::{Value, json};
use tokio::time::timeout;

use crate::{
    cancellation::{TurnCancelled, TurnExecutionContext},
    events::{AgentEventEmitter, AgentRawEvent, ToolExecutionOutcome},
    redaction::sanitize_text,
    types::{ChatMessage, Role, ToolCall},
};

use super::{
    ApprovalHandler, ToolCapability, ToolContext, ToolFailure, ToolInvocation, ToolRegistry,
    ToolSuccess, util::truncate_utf8,
};

#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    pub max_calls_per_round: usize,
    pub tool_timeout: Duration,
    pub max_output_bytes: usize,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            max_calls_per_round: 8,
            tool_timeout: Duration::from_secs(30),
            max_output_bytes: 64 * 1024,
        }
    }
}

pub struct ToolDispatcher {
    registry: Arc<ToolRegistry>,
    context: ToolContext,
    approval: Arc<dyn ApprovalHandler>,
    config: DispatcherConfig,
}

impl ToolDispatcher {
    pub fn new(
        registry: Arc<ToolRegistry>,
        workspace_root: impl AsRef<Path>,
        approval: Arc<dyn ApprovalHandler>,
        config: DispatcherConfig,
    ) -> Result<Self> {
        let context =
            ToolContext::new(workspace_root, config.tool_timeout, config.max_output_bytes)?;
        Ok(Self {
            registry,
            context,
            approval,
            config,
        })
    }

    pub fn registry(&self) -> &Arc<ToolRegistry> {
        &self.registry
    }

    pub fn workspace_root(&self) -> &Path {
        self.context.workspace_root()
    }

    #[allow(dead_code)]
    pub async fn dispatch(&self, calls: &[ToolCall]) -> Result<Vec<ChatMessage>> {
        self.dispatch_inner(calls, None).await
    }

    #[cfg(test)]
    pub(crate) async fn dispatch_with_events(
        &self,
        calls: &[ToolCall],
        events: &AgentEventEmitter,
    ) -> Result<Vec<ChatMessage>> {
        self.dispatch_inner(calls, Some(events)).await
    }

    pub(crate) async fn dispatch_with_context(
        &self,
        calls: &[ToolCall],
        events: &AgentEventEmitter,
        context: &TurnExecutionContext,
    ) -> Result<Vec<ChatMessage>> {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        self.dispatch_inner_with_context(calls, Some(events), Some(context))
            .await
    }

    async fn dispatch_inner(
        &self,
        calls: &[ToolCall],
        events: Option<&AgentEventEmitter>,
    ) -> Result<Vec<ChatMessage>> {
        self.dispatch_inner_with_context(calls, events, None).await
    }

    async fn dispatch_inner_with_context(
        &self,
        calls: &[ToolCall],
        events: Option<&AgentEventEmitter>,
        context: Option<&TurnExecutionContext>,
    ) -> Result<Vec<ChatMessage>> {
        if calls.len() > self.config.max_calls_per_round {
            anyhow::bail!(
                "Tool call limit exceeded: {} > {}",
                calls.len(),
                self.config.max_calls_per_round
            );
        }

        let mut results = vec![None; calls.len()];
        let mut index = 0;
        while index < calls.len() {
            let capability = self.capability_for(&calls[index]);
            if capability == ToolCapability::ReadOnly {
                let start = index;
                while index < calls.len()
                    && self.capability_for(&calls[index]) == ToolCapability::ReadOnly
                {
                    index += 1;
                }
                let futures = calls[start..index]
                    .iter()
                    .map(|call| self.execute_call(call, events, context));
                for (offset, message) in join_all(futures).await.into_iter().enumerate() {
                    results[start + offset] = Some(message?);
                }
            } else {
                results[index] = Some(self.execute_call(&calls[index], events, context).await?);
                index += 1;
            }
        }

        Ok(results.into_iter().flatten().collect())
    }

    fn capability_for(&self, call: &ToolCall) -> ToolCapability {
        self.registry
            .get(&call.function.name)
            .map(|tool| tool.capability())
            .unwrap_or(ToolCapability::Command)
    }

    async fn execute_call(
        &self,
        call: &ToolCall,
        events: Option<&AgentEventEmitter>,
        context: Option<&TurnExecutionContext>,
    ) -> Result<ChatMessage> {
        emit_event(
            events,
            AgentRawEvent::ToolExecutionStarted {
                call_id: call.id.clone().into(),
                name: call.function.name.clone().into(),
                arguments: call.function.arguments.clone().into(),
            },
        );
        let arguments = match serde_json::from_str::<Value>(&call.function.arguments) {
            Ok(arguments) => arguments,
            Err(error) => {
                return Ok(failed_tool_message(
                    events,
                    call,
                    ToolFailure::new(
                        "invalid_arguments",
                        format!("Arguments are not valid JSON: {error}"),
                    ),
                ));
            }
        };

        let Some(tool) = self.registry.get(&call.function.name) else {
            return Ok(failed_tool_message(
                events,
                call,
                ToolFailure::new(
                    "unknown_tool",
                    format!("Unknown tool: {}", call.function.name),
                ),
            ));
        };

        let invocation = ToolInvocation {
            call_id: call.id.clone(),
            name: call.function.name.clone(),
            arguments: arguments.clone(),
            capability: tool.capability(),
        };

        if invocation.capability != ToolCapability::ReadOnly {
            emit_event(
                events,
                AgentRawEvent::ToolApprovalRequested {
                    call_id: invocation.call_id.clone().into(),
                    name: invocation.name.clone().into(),
                    capability: invocation.capability,
                },
            );
            let approval = self.approval.approve(&invocation);
            let approval = match context {
                Some(context) => tokio::select! {
                    biased;
                    cancelled = context.cancelled() => Err(anyhow::Error::from(cancelled)),
                    approval = approval => approval,
                },
                None => approval.await,
            };
            match approval {
                Ok(true) => emit_event(
                    events,
                    AgentRawEvent::ToolApprovalResolved {
                        call_id: invocation.call_id.clone().into(),
                        approved: true,
                    },
                ),
                Ok(false) => {
                    emit_event(
                        events,
                        AgentRawEvent::ToolApprovalResolved {
                            call_id: invocation.call_id.clone().into(),
                            approved: false,
                        },
                    );
                    return Ok(failed_tool_message(
                        events,
                        call,
                        ToolFailure::new("approval_denied", "User denied tool execution"),
                    ));
                }
                Err(error) if error.downcast_ref::<TurnCancelled>().is_some() => {
                    emit_event(
                        events,
                        AgentRawEvent::ToolApprovalResolved {
                            call_id: invocation.call_id.clone().into(),
                            approved: false,
                        },
                    );
                    emit_event(
                        events,
                        AgentRawEvent::ToolExecutionEnded {
                            call_id: call.id.clone().into(),
                            name: call.function.name.clone().into(),
                            outcome: ToolExecutionOutcome::Cancelled.into(),
                        },
                    );
                    return Err(error);
                }
                Err(error) => {
                    emit_event(
                        events,
                        AgentRawEvent::ToolApprovalResolved {
                            call_id: invocation.call_id.clone().into(),
                            approved: false,
                        },
                    );
                    return Ok(failed_tool_message(
                        events,
                        call,
                        ToolFailure::new("approval_failed", error.to_string()),
                    ));
                }
            }
        }

        tracing::debug!(
            tool = %sanitize_text(&invocation.name, 256),
            call_id_bytes = invocation.call_id.len(),
            arguments_bytes = invocation.arguments.to_string().len(),
            "Executing tool"
        );
        let execution = timeout(
            self.config.tool_timeout,
            tool.execute(&self.context, arguments),
        );
        let execution = match context {
            Some(context) => tokio::select! {
                biased;
                cancelled = context.cancelled() => {
                    emit_event(
                        events,
                        AgentRawEvent::ToolExecutionEnded {
                            call_id: call.id.clone().into(),
                            name: call.function.name.clone().into(),
                            outcome: ToolExecutionOutcome::Cancelled.into(),
                        },
                    );
                    return Err(cancelled.into());
                },
                execution = execution => execution,
            },
            None => execution.await,
        };
        let (envelope, outcome) = match execution {
            Ok(Ok(success)) => success_envelope(success, self.config.max_output_bytes),
            Ok(Err(error)) => {
                let outcome = failure_outcome(&error);
                (failure_envelope(error), outcome)
            }
            Err(_) => {
                let error = ToolFailure::new(
                    "timeout",
                    format!(
                        "Tool exceeded {} second timeout",
                        self.config.tool_timeout.as_secs()
                    ),
                );
                (failure_envelope(error), ToolExecutionOutcome::TimedOut)
            }
        };
        emit_event(
            events,
            AgentRawEvent::ToolExecutionEnded {
                call_id: call.id.clone().into(),
                name: call.function.name.clone().into(),
                outcome: outcome.into(),
            },
        );
        Ok(tool_message(call, envelope))
    }
}

fn emit_event(events: Option<&AgentEventEmitter>, event: AgentRawEvent) {
    if let Some(events) = events {
        events.emit(event);
    }
}

fn failed_tool_message(
    events: Option<&AgentEventEmitter>,
    call: &ToolCall,
    error: ToolFailure,
) -> ChatMessage {
    let outcome = failure_outcome(&error);
    emit_event(
        events,
        AgentRawEvent::ToolExecutionEnded {
            call_id: call.id.clone().into(),
            name: call.function.name.clone().into(),
            outcome: outcome.into(),
        },
    );
    tool_message(call, failure_envelope(error))
}

fn failure_outcome(error: &ToolFailure) -> ToolExecutionOutcome {
    match error.code.as_str() {
        "timeout" => ToolExecutionOutcome::TimedOut,
        "approval_denied" => ToolExecutionOutcome::ApprovalDenied,
        _ => ToolExecutionOutcome::Failed {
            code: error.code.clone(),
            message: error.message.clone(),
        },
    }
}

fn tool_message(call: &ToolCall, envelope: Value) -> ChatMessage {
    ChatMessage {
        role: Role::Tool,
        content: Some(envelope.to_string()),
        reasoning: None,
        tool_calls: None,
        tool_call_id: Some(call.id.clone()),
        name: Some(call.function.name.clone()),
    }
}

fn success_envelope(mut success: ToolSuccess, max_bytes: usize) -> (Value, ToolExecutionOutcome) {
    let serialized = success.output.to_string();
    if serialized.len() > max_bytes {
        let (output, _) = truncate_utf8(&serialized, max_bytes);
        success.output = Value::String(output);
        success.truncated = true;
    }
    let outcome = ToolExecutionOutcome::Succeeded {
        output: success.output.clone(),
        truncated: success.truncated,
    };
    let envelope = json!({
        "ok": true,
        "output": success.output,
        "truncated": success.truncated,
    });
    (envelope, outcome)
}

fn failure_envelope(error: ToolFailure) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": error.code,
            "message": error.message,
            "details": error.details,
        }
    })
}
