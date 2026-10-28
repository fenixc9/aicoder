use crate::run::RunLimiter;
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use futures::future::join_all;
use serde_json::{Value, json};
use tokio::time::timeout;

use crate::{
    cancellation::{TurnCancelled, TurnExecutionContext},
    events::{AgentEventEmitter, AgentRawEvent, ToolExecutionOutcome},
    model::{ContentBlock, Message, MessageRole, ModelToolCall},
    redaction::sanitize_text,
};

use super::{
    ApprovalHandler, ToolCapability, ToolContext, ToolFailure, ToolInvocation, ToolRegistry,
    ToolSuccess, util::truncate_utf8,
};

pub(crate) struct DispatchCapture(Mutex<Vec<Option<Message>>>);
impl DispatchCapture {
    pub fn new(count: usize) -> Self {
        Self(Mutex::new(vec![None; count]))
    }
    pub fn messages(&self) -> Vec<Message> {
        self.0
            .lock()
            .expect("tool progress poisoned")
            .iter_mut()
            .filter_map(Option::take)
            .collect()
    }
}

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
    pub async fn dispatch(&self, calls: &[ModelToolCall]) -> Result<Vec<Message>> {
        self.dispatch_inner(calls, None).await
    }

    #[cfg(test)]
    pub(crate) async fn dispatch_with_events(
        &self,
        calls: &[ModelToolCall],
        events: &AgentEventEmitter,
    ) -> Result<Vec<Message>> {
        self.dispatch_inner(calls, Some(events)).await
    }

    async fn dispatch_inner(
        &self,
        calls: &[ModelToolCall],
        events: Option<&AgentEventEmitter>,
    ) -> Result<Vec<Message>> {
        self.dispatch_inner_with_context(calls, events, None, None)
            .await
    }

    pub fn max_calls_per_round(&self) -> usize {
        self.config.max_calls_per_round
    }
    pub(crate) async fn dispatch_captured(
        &self,
        calls: &[ModelToolCall],
        events: &AgentEventEmitter,
        context: &TurnExecutionContext,
        limits: &Arc<RunLimiter>,
        capture: &DispatchCapture,
    ) -> Result<Vec<Message>> {
        self.dispatch_inner_with_context(
            calls,
            Some(events),
            Some(context),
            Some((limits, capture)),
        )
        .await
    }
    async fn execute_recorded(
        &self,
        index: usize,
        call: &ModelToolCall,
        events: Option<&AgentEventEmitter>,
        context: Option<&TurnExecutionContext>,
        control: Option<(&Arc<RunLimiter>, &DispatchCapture)>,
    ) -> Result<Message> {
        let result = self
            .execute_call(
                call,
                events,
                context,
                control.map(|(limits, _)| limits.as_ref()),
            )
            .await;
        if let (Some((_, capture)), Ok(message)) = (control, &result) {
            capture.0.lock().expect("tool progress poisoned")[index] = Some(message.clone());
        }
        result
    }
    async fn dispatch_inner_with_context(
        &self,
        calls: &[ModelToolCall],
        events: Option<&AgentEventEmitter>,
        context: Option<&TurnExecutionContext>,
        control: Option<(&Arc<RunLimiter>, &DispatchCapture)>,
    ) -> Result<Vec<Message>> {
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
                    .enumerate()
                    .map(|(offset, call)| {
                        self.execute_recorded(start + offset, call, events, context, control)
                    });
                for (offset, message) in join_all(futures).await.into_iter().enumerate() {
                    results[start + offset] = Some(message?);
                }
            } else {
                results[index] = Some(
                    self.execute_recorded(index, &calls[index], events, context, control)
                        .await?,
                );
                index += 1;
            }
        }

        Ok(results.into_iter().flatten().collect())
    }

    fn capability_for(&self, call: &ModelToolCall) -> ToolCapability {
        self.registry
            .get(&call.name)
            .map(|tool| tool.capability())
            .unwrap_or(ToolCapability::Command)
    }

    async fn execute_call(
        &self,
        call: &ModelToolCall,
        events: Option<&AgentEventEmitter>,
        context: Option<&TurnExecutionContext>,
        limits: Option<&RunLimiter>,
    ) -> Result<Message> {
        if let Some(context) = context
            && context.is_cancelled()
        {
            return Err(context.error().into());
        }
        if let Some(limits) = limits {
            limits.tool_started();
        }
        emit_event(
            events,
            AgentRawEvent::ToolExecutionStarted {
                call_id: call.id.clone().into(),
                name: call.name.clone().into(),
                arguments: call.arguments.to_string().into(),
            },
        );
        let arguments = call.arguments.clone();

        let Some(tool) = self.registry.get(&call.name) else {
            return Ok(failed_tool_message(
                events,
                call,
                limits,
                ToolFailure::new("unknown_tool", format!("Unknown tool: {}", call.name)),
            ));
        };

        let invocation = ToolInvocation {
            call_id: call.id.clone(),
            name: call.name.clone(),
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
                        limits,
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
                            name: call.name.clone().into(),
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
                        limits,
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
                            name: call.name.clone().into(),
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
        let (envelope, outcome) = limit_payload(envelope, outcome, limits);
        emit_event(
            events,
            AgentRawEvent::ToolExecutionEnded {
                call_id: call.id.clone().into(),
                name: call.name.clone().into(),
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
    call: &ModelToolCall,
    limits: Option<&RunLimiter>,
    error: ToolFailure,
) -> Message {
    let outcome = failure_outcome(&error);
    let (envelope, outcome) = limit_payload(failure_envelope(error), outcome, limits);
    emit_event(
        events,
        AgentRawEvent::ToolExecutionEnded {
            call_id: call.id.clone().into(),
            name: call.name.clone().into(),
            outcome: outcome.into(),
        },
    );
    tool_message(call, envelope)
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

fn tool_message(call: &ModelToolCall, envelope: Value) -> Message {
    Message {
        role: MessageRole::Tool,
        content: vec![ContentBlock::ToolResult {
            call_id: call.id.clone(),
            is_error: envelope["ok"] == false,
            content: envelope,
        }],
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

fn limit_payload(
    mut envelope: Value,
    mut outcome: ToolExecutionOutcome,
    limits: Option<&RunLimiter>,
) -> (Value, ToolExecutionOutcome) {
    if let Some(limits) = limits {
        let is_success = envelope["ok"] == true;
        let payload = if is_success {
            &envelope["output"]
        } else {
            &envelope["error"]
        };
        let encoded = payload.to_string();
        let retained = limits.retain_text(&encoded);
        if retained.len() != encoded.len() {
            if is_success {
                envelope["output"] = json!(retained);
                envelope["truncated"] = json!(true);
                outcome = ToolExecutionOutcome::Succeeded {
                    output: envelope["output"].clone(),
                    truncated: true,
                };
            } else {
                envelope["error"]["message"] = json!(retained);
                envelope["error"]["details"] = Value::Null;
                if let ToolExecutionOutcome::Failed { message, .. } = &mut outcome {
                    *message = envelope["error"]["message"].as_str().unwrap().into();
                }
            }
        }
    }
    (envelope, outcome)
}
