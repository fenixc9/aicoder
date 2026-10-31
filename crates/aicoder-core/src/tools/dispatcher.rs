use crate::run::{RunFailure, RunLimiter, RunStage};
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
};

#[derive(Clone, Copy)]
pub(crate) struct DispatchJournal<'a> {
    pub journal: &'a dyn crate::RunJournal,
    pub base_index: usize,
}
pub(crate) struct DispatchCapture(Mutex<Vec<Option<Message>>>, Mutex<Option<RunFailure>>);
impl DispatchCapture {
    pub fn new(count: usize) -> Self {
        Self(Mutex::new(vec![None; count]), Mutex::new(None))
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
    hooks: Vec<crate::extensions::Registered<dyn crate::extensions::ToolHook>>,
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
            hooks: Vec::new(),
            registry,
            context,
            approval,
            config,
        })
    }

    pub(crate) fn with_tool_hooks(
        mut self,
        hooks: Vec<crate::extensions::Registered<dyn crate::extensions::ToolHook>>,
    ) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn with_artifact_store(
        mut self,
        store: Arc<dyn crate::artifacts::ArtifactStore>,
    ) -> Result<Self> {
        store.limits().validate()?;
        anyhow::ensure!(
            self.config.max_output_bytes >= 512,
            "Artifact tool output budget must be at least 512 bytes"
        );
        self.context = self.context.with_artifact_store(store);
        Ok(self)
    }

    async fn failed_tool_message(
        &self,
        events: Option<&AgentEventEmitter>,
        call: &ModelToolCall,
        limits: Option<&RunLimiter>,
        error: ToolFailure,
    ) -> Message {
        let error = super::output::bound_failure(error, &self.context).await;
        let artifacts = error.artifacts.clone();
        let outcome = failure_outcome(&error);
        let (envelope, outcome) = limit_payload(failure_envelope(error), outcome, limits);
        emit_event(
            events,
            AgentRawEvent::ToolExecutionEnded {
                artifacts,
                call_id: call.id.clone().into(),
                name: call.name.clone().into(),
                outcome: outcome.into(),
            },
        );
        tool_message(call, envelope)
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

    /// Dispatch a standalone batch with tool events. Agent hosts should use the executor
    /// for run control, budgets and durable journaling.
    pub async fn dispatch_with_events(
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
        journal: DispatchJournal<'_>,
    ) -> Result<Vec<Message>> {
        let child = context.child();
        let result = self
            .dispatch_inner_with_context(
                calls,
                Some(events),
                Some(&child),
                Some((limits, capture, Some(journal))),
            )
            .await;
        if let Some(failure) = capture.1.lock().expect("tool progress poisoned").clone() {
            return Err(failure.into());
        }
        result
    }
    async fn execute_recorded(
        &self,
        index: usize,
        call: &ModelToolCall,
        events: Option<&AgentEventEmitter>,
        context: Option<&TurnExecutionContext>,
        control: Option<(
            &Arc<RunLimiter>,
            &DispatchCapture,
            Option<DispatchJournal<'_>>,
        )>,
    ) -> Result<Message> {
        let result = if context.is_some_and(|context| {
            context
                .control
                .as_ref()
                .is_some_and(|control| control.has_steering())
        }) {
            emit_event(
                events,
                AgentRawEvent::ToolExecutionStarted {
                    call_id: call.id.clone().into(),
                    name: call.name.clone().into(),
                    arguments: call.arguments.to_string().into(),
                },
            );
            Ok(self
                .failed_tool_message(
                    events,
                    call,
                    control.map(|(limits, _, _)| limits.as_ref()),
                    ToolFailure::new(
                        "steered_before_execution",
                        "Tool was not executed because steering input is waiting",
                    ),
                )
                .await)
        } else {
            self.execute_call(
                call,
                events,
                context,
                control.map(|(limits, _, _)| limits.as_ref()),
            )
            .await
        };
        if let (Some((_, capture, journal)), Ok(message)) = (control, &result) {
            capture.0.lock().expect("tool progress poisoned")[index] = Some(message.clone());
            if let Some(journal) = journal
                && let Err(error) = journal
                    .journal
                    .checkpoint(journal.base_index + index, message)
                    .await
            {
                let failure = RunFailure::from_error(RunStage::Persistence, &error);
                *capture.1.lock().expect("tool progress poisoned") = Some(failure.clone());
                if let Some(context) = context {
                    context.cancel("session persistence failed");
                }
                return Err(failure.into());
            }
        }
        result
    }
    async fn dispatch_inner_with_context(
        &self,
        calls: &[ModelToolCall],
        events: Option<&AgentEventEmitter>,
        context: Option<&TurnExecutionContext>,
        control: Option<(
            &Arc<RunLimiter>,
            &DispatchCapture,
            Option<DispatchJournal<'_>>,
        )>,
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
            return Ok(self
                .failed_tool_message(
                    events,
                    call,
                    limits,
                    ToolFailure::new("unknown_tool", format!("Unknown tool: {}", call.name)),
                )
                .await);
        };

        if !self.hooks.is_empty() {
            let checking = timeout(
                self.config.tool_timeout,
                crate::extensions::tool_hooks(
                    &self.hooks,
                    self.context.workspace_root(),
                    call,
                    tool.capability(),
                ),
            );
            let checked = match context {
                Some(context) => tokio::select! {biased;
                    cancelled=context.cancelled()=>{
                        emit_event(events,AgentRawEvent::ToolExecutionEnded {artifacts:Vec::new(),call_id:call.id.clone().into(),name:call.name.clone().into(),outcome:ToolExecutionOutcome::Cancelled.into()});
                        return Err(cancelled.into());
                    },
                    _=async {match &context.control {Some(control)=>control.steering_requested().await,None=>std::future::pending().await}}=>{
                        return Ok(self.failed_tool_message(events,call,limits,ToolFailure::new("steered_before_execution","Tool hook was superseded by steering")).await);
                    },
                    result=checking=>result,
                },
                None => checking.await,
            };
            let failure = match checked {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(_) => Some(ToolFailure::new(
                    "timeout",
                    "Tool hooks exceeded their phase time limit",
                )),
            };
            if let Some(failure) = failure {
                return Ok(self
                    .failed_tool_message(events, call, limits, failure)
                    .await);
            }
        }

        // Prepare once, before approval, without changing external state. The same
        // owned operation is consumed after approval; no re-reading/re-planning then.
        let preparation = timeout(
            self.config.tool_timeout,
            tool.prepare(&self.context, arguments.clone()),
        );
        let prepared = match context {
            Some(context) => tokio::select! {
                biased;
                cancelled = context.cancelled() => {
                    emit_event(events, AgentRawEvent::ToolExecutionEnded {
                            artifacts:Vec::new(),
                        call_id: call.id.clone().into(), name: call.name.clone().into(),
                        outcome: ToolExecutionOutcome::Cancelled.into(),
                    });
                    return Err(cancelled.into());
                },
                _ = async { match &context.control { Some(control) => control.steering_requested().await, None => std::future::pending().await } } => {
                    return Ok(self.failed_tool_message(events,call,limits,ToolFailure::new("steered_before_execution","Tool preparation was superseded by steering")).await);
                },
                result = preparation => result,
            },
            None => preparation.await,
        };
        let prepared = match prepared {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => {
                return Ok(self.failed_tool_message(events, call, limits, error).await);
            }
            Err(_) => {
                return Ok(self
                    .failed_tool_message(
                        events,
                        call,
                        limits,
                        ToolFailure::new("timeout", "Tool preparation exceeded its time limit"),
                    )
                    .await);
            }
        };
        let preview = prepared.as_ref().map(|plan| plan.preview());
        if preview.as_ref().is_some_and(|preview| {
            serde_json::to_vec(preview)
                .map_or(true, |bytes| bytes.len() > super::MAX_TOOL_PREVIEW_BYTES)
        }) {
            return Ok(self
                .failed_tool_message(
                    events,
                    call,
                    limits,
                    ToolFailure::new("preview_too_large", "Prepared tool preview exceeds 32 KiB"),
                )
                .await);
        }
        let invocation = ToolInvocation {
            call_id: call.id.clone(),
            name: call.name.clone(),
            arguments: arguments.clone(),
            capability: tool.capability(),
            preview,
        };

        let ticket = if invocation.capability != ToolCapability::ReadOnly {
            match context
                .and_then(|context| context.approval_session.as_ref())
                .map(|session| session.ticket(self.context.workspace_root()))
                .transpose()
            {
                Ok(ticket) => ticket,
                Err(error) => {
                    return Ok(self
                        .failed_tool_message(
                            events,
                            call,
                            limits,
                            ToolFailure::new(error.code(), error.to_string()),
                        )
                        .await);
                }
            }
        } else {
            None
        };
        if invocation.capability != ToolCapability::ReadOnly {
            emit_event(
                events,
                AgentRawEvent::ToolApprovalRequested {
                    call_id: invocation.call_id.clone().into(),
                    name: invocation.name.clone().into(),
                    capability: invocation.capability,
                    preview: invocation.preview.clone().map(Arc::new),
                    session: ticket.as_ref().map(|ticket| Arc::new(ticket.info.clone())),
                },
            );
            let cached = ticket
                .as_ref()
                .and_then(|ticket| ticket.lookup(self.registry.authority, &invocation));
            let approval = async {
                let request = super::ApprovalRequest {
                    invocation: &invocation,
                    session: ticket.as_ref().map(|ticket| ticket.info.clone()),
                };
                let (decision, source) = if cached.is_some() {
                    (
                        super::ApprovalDecision::AllowSession,
                        super::ApprovalSource::SessionGrant,
                    )
                } else if let Some(control) = context
                    .and_then(|context| context.control.as_ref())
                    .filter(|control| control.handle_approvals())
                {
                    (
                        control.approve(&request, events).await?,
                        super::ApprovalSource::RunHandle,
                    )
                } else {
                    (
                        self.approval.decide(&request).await?,
                        super::ApprovalSource::Handler,
                    )
                };
                if let Some(context) = context {
                    if context.is_cancelled() {
                        return Err(context.error().into());
                    }
                    if context
                        .control
                        .as_ref()
                        .is_some_and(|control| control.has_steering())
                    {
                        return Err(crate::control::SteeringRequested.into());
                    }
                }
                let grant_id = match &ticket {
                    Some(ticket) => ticket.apply(self.registry.authority, &invocation, decision)?,
                    None if decision == super::ApprovalDecision::AllowSession => {
                        return Err(super::ApprovalError::SessionUnavailable.into());
                    }
                    None => None,
                };
                Ok::<_, anyhow::Error>(super::ApprovalResolution {
                    decision,
                    source,
                    session: request.session,
                    grant_id,
                })
            };
            let approval = match context {
                Some(context) => tokio::select! {biased;
                    cancelled=context.cancelled()=>Err(anyhow::Error::from(cancelled)),
                    _=async{match &context.control{Some(control)=>control.steering_requested().await,None=>std::future::pending().await}}=>Err(crate::control::SteeringRequested.into()),
                    revoked=async{match &ticket { Some(ticket)=>ticket.revoked().await, None=>std::future::pending().await }}=>Err(anyhow::Error::from(revoked)),
                    approval=approval=>approval,
                },
                None => approval.await,
            };
            match approval {
                Ok(resolution) => {
                    let approved = resolution.decision.is_allowed();
                    emit_event(
                        events,
                        AgentRawEvent::ToolApprovalResolved {
                            call_id: invocation.call_id.clone().into(),
                            approved,
                            resolution: Some(Arc::new(resolution)),
                        },
                    );
                    if !approved {
                        return Ok(self
                            .failed_tool_message(
                                events,
                                call,
                                limits,
                                ToolFailure::new("approval_denied", "User denied tool execution"),
                            )
                            .await);
                    }
                }
                Err(error) => {
                    emit_event(
                        events,
                        AgentRawEvent::ToolApprovalResolved {
                            call_id: invocation.call_id.clone().into(),
                            approved: false,
                            resolution: None,
                        },
                    );
                    if error.downcast_ref::<TurnCancelled>().is_some() {
                        emit_event(
                            events,
                            AgentRawEvent::ToolExecutionEnded {
                                artifacts: Vec::new(),
                                call_id: call.id.clone().into(),
                                name: call.name.clone().into(),
                                outcome: ToolExecutionOutcome::Cancelled.into(),
                            },
                        );
                        return Err(error);
                    }
                    let failure = if error
                        .downcast_ref::<crate::control::SteeringRequested>()
                        .is_some()
                    {
                        ToolFailure::new(
                            "steered_before_execution",
                            "Approval was superseded by steering; tool was not executed",
                        )
                    } else if let Some(error) = error.downcast_ref::<super::ApprovalError>() {
                        ToolFailure::new(error.code(), error.to_string())
                    } else {
                        ToolFailure::new("approval_failed", error.to_string())
                    };
                    return Ok(self
                        .failed_tool_message(events, call, limits, failure)
                        .await);
                }
            }
        }
        if let Some(ticket) = &ticket
            && let Err(error) = ticket.current()
        {
            return Ok(self
                .failed_tool_message(
                    events,
                    call,
                    limits,
                    ToolFailure::new(error.code(), error.to_string()),
                )
                .await);
        }

        if context.is_some_and(|context| {
            context
                .control
                .as_ref()
                .is_some_and(|control| control.has_steering())
        }) {
            return Ok(self
                .failed_tool_message(
                    events,
                    call,
                    limits,
                    ToolFailure::new(
                        "steered_before_execution",
                        "Tool was superseded by steering before execution",
                    ),
                )
                .await);
        }

        tracing::debug!(
            tool = %sanitize_text(&invocation.name, 256),
            call_id_bytes = invocation.call_id.len(),
            arguments_bytes = invocation.arguments.to_string().len(),
            "Executing tool"
        );
        let execution = timeout(self.config.tool_timeout, async {
            match prepared {
                Some(plan) => plan.execute(&self.context).await,
                None => tool.execute(&self.context, arguments).await,
            }
        });
        let execution = match context {
            Some(context) => tokio::select! {
                biased;
                cancelled = context.cancelled() => {
                    emit_event(
                        events,
                        AgentRawEvent::ToolExecutionEnded {
                            artifacts:Vec::new(),
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
            Ok(Ok(success)) => super::output::success_envelope(success, &self.context).await,
            Ok(Err(error)) => {
                let error = super::output::bound_failure(error, &self.context).await;
                let outcome = failure_outcome(&error);
                (failure_envelope(error), outcome)
            }
            Err(_) => {
                let error = ToolFailure::new(
                    "timeout",
                    format!(
                        "Tool exceeded {} ms timeout",
                        self.config.tool_timeout.as_millis()
                    ),
                );
                (failure_envelope(error), ToolExecutionOutcome::TimedOut)
            }
        };
        let artifacts = envelope
            .get("artifacts")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        let (envelope, outcome) = limit_payload(envelope, outcome, limits);
        emit_event(
            events,
            AgentRawEvent::ToolExecutionEnded {
                artifacts,
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

fn failure_envelope(error: ToolFailure) -> Value {
    let mut envelope = json!({"ok":false,"error":{"code":error.code,"message":error.message,"details":error.details}});
    if !error.artifacts.is_empty() {
        envelope["artifacts"] = json!(error.artifacts);
    }
    envelope
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
