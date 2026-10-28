use super::{AgentEventEmitter, AgentRawEvent, StreamEnd, ToolCallKey};
use crate::{
    model::{ContentBlock, ModelEvent, ModelEventSink, ModelResponse, ModelToolCall, ModelUsage},
    run::{PartialModelResponse, PartialToolCall, RunLimiter},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Default)]
struct Progress {
    text: bool,
    reasoning: bool,
    tools: BTreeSet<u32>,
    completed: BTreeSet<u32>,
    usage: Option<ModelUsage>,
    closed: bool,
    retries: usize,
    reported: bool,
    partial: PartialModelResponse,
    calls: BTreeMap<u32, PartialToolCall>,
}
pub(crate) struct RunModelEventSink {
    events: AgentEventEmitter,
    progress: Mutex<Progress>,
    limits: Option<Arc<RunLimiter>>,
    retry_cost: u64,
}
impl RunModelEventSink {
    #[cfg(test)]
    pub(crate) fn new(events: AgentEventEmitter) -> Self {
        Self {
            events,
            progress: Mutex::new(Progress::default()),
            limits: None,
            retry_cost: 0,
        }
    }
    pub(crate) fn with_limits(
        events: AgentEventEmitter,
        limits: Arc<RunLimiter>,
        retry_cost: u64,
    ) -> Self {
        Self {
            events,
            progress: Mutex::new(Progress::default()),
            limits: Some(limits),
            retry_cost,
        }
    }
    fn retain(&self, text: &str) -> String {
        self.limits
            .as_ref()
            .map_or_else(|| text.to_string(), |limits| limits.retain_text(text))
    }
    fn limited(&self) -> bool {
        self.limits
            .as_ref()
            .is_some_and(|limits| limits.violation().is_some())
    }
    fn text(&self, state: &mut Progress, text: &str, reasoning: bool) {
        let text = self.retain(text);
        if text.is_empty() {
            return;
        }
        if reasoning {
            if !state.reasoning {
                state.reasoning = true;
                self.events
                    .emit(AgentRawEvent::ReasoningStarted { response_index: 0 });
            }
            state.partial.reasoning.push_str(&text);
            self.events.emit(AgentRawEvent::ReasoningChunk {
                response_index: 0,
                delta: text.into(),
            });
        } else {
            if !state.text {
                state.text = true;
                self.events
                    .emit(AgentRawEvent::ContentStarted { response_index: 0 });
            }
            state.partial.text.push_str(&text);
            self.events.emit(AgentRawEvent::ContentChunk {
                response_index: 0,
                delta: text.into(),
            });
        }
    }
    fn call(&self, state: &mut Progress, index: u32, call: ModelToolCall) {
        if index > i32::MAX as u32 || state.completed.contains(&index) {
            return;
        }
        let key = ToolCallKey {
            response_index: 0,
            tool_index: index as i32,
        };
        if !state.tools.contains(&index) {
            self.events.emit(AgentRawEvent::ToolCallStarted { key });
            state.tools.insert(index);
        }
        let fragment = state.calls.entry(index).or_insert_with(|| PartialToolCall {
            index,
            ..Default::default()
        });
        let arguments = call.arguments.to_string();
        for (target, full) in [
            (&mut fragment.id, call.id.as_str()),
            (&mut fragment.name, call.name.as_str()),
            (&mut fragment.arguments, arguments.as_str()),
        ] {
            if target.len() < full.len() {
                let start = if full.is_char_boundary(target.len()) {
                    target.len()
                } else {
                    0
                };
                target.push_str(&self.retain(&full[start..]));
            }
        }
        if self.limited() {
            return;
        }
        fragment.id = call.id.clone();
        fragment.name = call.name.clone();
        fragment.arguments = arguments;
        state.tools.remove(&index);
        state.completed.insert(index);
        self.events.emit(AgentRawEvent::ToolCallEnded {
            key,
            outcome: StreamEnd::Completed,
            tool_call: Some(call.into()),
        });
    }
    fn close(&self, state: &mut Progress, outcome: StreamEnd) {
        if state.text {
            self.events.emit(AgentRawEvent::ContentEnded {
                response_index: 0,
                outcome: outcome.clone(),
            });
        }
        if state.reasoning {
            self.events.emit(AgentRawEvent::ReasoningEnded {
                response_index: 0,
                outcome: outcome.clone(),
            });
        }
        for index in std::mem::take(&mut state.tools) {
            self.events.emit(AgentRawEvent::ToolCallEnded {
                key: ToolCallKey {
                    response_index: 0,
                    tool_index: index as i32,
                },
                outcome: outcome.clone(),
                tool_call: None,
            });
        }
        state.closed = true;
    }
    pub(crate) fn finish(&self, response: &ModelResponse) {
        let mut state = self.progress.lock().expect("model progress poisoned");
        if state.closed {
            return;
        }
        let mut text_offset = state.partial.text.len();
        let mut reasoning_offset = state.partial.reasoning.len();
        let mut tool_index = 0;
        for block in &response.message.content {
            match block {
                ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                    let reasoning = matches!(block, ContentBlock::Reasoning { .. });
                    let offset = if reasoning {
                        &mut reasoning_offset
                    } else {
                        &mut text_offset
                    };
                    let skip = (*offset).min(text.len());
                    *offset -= skip;
                    if text.is_char_boundary(skip) {
                        self.text(&mut state, &text[skip..], reasoning);
                    }
                }
                ContentBlock::ToolCall { call } => {
                    self.call(&mut state, tool_index, call.clone());
                    tool_index += 1;
                }
                other => {
                    let _ = self.retain(&serde_json::to_string(other).unwrap_or_default());
                }
            }
        }
        let end = if self.limited() {
            StreamEnd::Aborted {
                reason: "run budget reached".into(),
            }
        } else {
            StreamEnd::Completed
        };
        self.close(&mut state, end);
        state.reported |= crate::run::usage_reported(&response.usage);
        state.usage = Some(if state.retries > 0 {
            ModelUsage::default()
        } else {
            response.usage.clone()
        });
        self.events.emit(AgentRawEvent::UsageUpdated {
            usage: state.usage.clone().unwrap_or_default().into(),
        });
        self.events.emit(AgentRawEvent::ModelResponseCompleted {
            finish_reason: Some(response.stop_reason.clone()),
        });
    }
    pub(crate) fn record_usage(&self, usage: &ModelUsage) {
        let mut state = self.progress.lock().expect("model progress poisoned");
        state.reported |= crate::run::usage_reported(usage);
        state.usage = Some(usage.clone());
    }
    pub(crate) fn reported_usage(&self) -> bool {
        self.progress
            .lock()
            .expect("model progress poisoned")
            .reported
    }
    pub(crate) fn usage(&self) -> ModelUsage {
        let state = self.progress.lock().expect("model progress poisoned");
        if state.retries > 0 {
            ModelUsage::default()
        } else {
            state.usage.clone().unwrap_or_default()
        }
    }
    pub(crate) fn retained(&self) -> (ModelUsage, PartialModelResponse) {
        let mut state = self.progress.lock().expect("model progress poisoned");
        let mut partial = std::mem::take(&mut state.partial);
        partial.tool_calls = std::mem::take(&mut state.calls).into_values().collect();
        (
            if state.retries > 0 {
                ModelUsage::default()
            } else {
                state.usage.clone().unwrap_or_default()
            },
            partial,
        )
    }
    pub(crate) fn abort(&self, reason: &str) -> (ModelUsage, PartialModelResponse) {
        {
            let mut state = self.progress.lock().expect("model progress poisoned");
            if !state.closed {
                self.close(
                    &mut state,
                    StreamEnd::Aborted {
                        reason: reason.into(),
                    },
                );
                self.events.emit(AgentRawEvent::UsageUpdated {
                    usage: if state.retries > 0 {
                        ModelUsage::default()
                    } else {
                        state.usage.clone().unwrap_or_default()
                    }
                    .into(),
                });
            }
        }
        self.retained()
    }
}
impl ModelEventSink for RunModelEventSink {
    fn emit(&self, event: ModelEvent) {
        let mut state = self.progress.lock().expect("model progress poisoned");
        if state.closed {
            return;
        }
        match event {
            ModelEvent::Started => self.events.emit(AgentRawEvent::ModelResponseStarted),
            ModelEvent::TextDelta { delta } => self.text(&mut state, &delta, false),
            ModelEvent::ReasoningDelta { delta } => self.text(&mut state, &delta, true),
            ModelEvent::ToolCallDelta {
                index,
                id_delta,
                name_delta,
                arguments_delta,
            } => {
                if index > i32::MAX as u32 {
                    return;
                }
                let key = ToolCallKey {
                    response_index: 0,
                    tool_index: index as i32,
                };
                if state.tools.insert(index) {
                    self.events.emit(AgentRawEvent::ToolCallStarted { key });
                }
                let id_delta = id_delta.map(|s| self.retain(&s));
                let name_delta = name_delta.map(|s| self.retain(&s));
                let arguments_delta = arguments_delta.map(|s| self.retain(&s));
                let fragment = state.calls.entry(index).or_insert_with(|| PartialToolCall {
                    index,
                    ..Default::default()
                });
                if let Some(s) = &id_delta {
                    fragment.id.push_str(s);
                }
                if let Some(s) = &name_delta {
                    fragment.name.push_str(s);
                }
                if let Some(s) = &arguments_delta {
                    fragment.arguments.push_str(s);
                }
                self.events.emit(AgentRawEvent::ToolCallChunk {
                    key,
                    id_delta: id_delta.map(Into::into),
                    name_delta: name_delta.map(Into::into),
                    arguments_delta: arguments_delta.map(Into::into),
                });
            }
            ModelEvent::ToolCallCompleted { index, call } => self.call(&mut state, index, call),
            ModelEvent::Usage { usage } => {
                state.reported |= crate::run::usage_reported(&usage);
                state.usage = Some(usage);
            }
            ModelEvent::Retry {
                attempt,
                delay_ms,
                reason,
            } => {
                state.retries += 1;
                if let Some(limits) = &self.limits {
                    limits.account_tokens(&ModelUsage::default(), self.retry_cost);
                    if limits
                        .remaining_tokens()
                        .is_some_and(|remaining| remaining < self.retry_cost)
                    {
                        let limit = limits.budget.max_tokens.unwrap();
                        let remaining = limits.remaining_tokens().unwrap();
                        limits.stop(crate::BudgetExceeded {
                            resource: crate::BudgetResource::Tokens,
                            limit,
                            used: limit.saturating_sub(remaining),
                            requested: Some(self.retry_cost),
                        });
                    }
                }
                self.events.emit(AgentRawEvent::ModelRetryScheduled {
                    attempt,
                    delay: Duration::from_millis(delay_ms),
                    reason: reason.into(),
                });
            }
            ModelEvent::Finished { .. } | ModelEvent::Aborted { .. } => {}
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Message, MessageRole, StopReason};
    use std::sync::Arc;

    #[tokio::test]
    async fn usage_snapshots_are_counted_once_and_late_provider_events_are_ignored() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let target = recorded.clone();
        let events = AgentEventEmitter::new(Arc::new(
            move |event: &super::super::AgentRawEventEnvelope| {
                target.lock().unwrap().push(event.event.clone());
            },
        ));
        let sink = RunModelEventSink::new(events.clone());
        sink.emit(ModelEvent::Started);
        sink.emit(ModelEvent::Usage {
            usage: ModelUsage {
                total_tokens: Some(5),
                ..Default::default()
            },
        });
        let usage = ModelUsage {
            total_tokens: Some(7),
            ..Default::default()
        };
        sink.emit(ModelEvent::Usage {
            usage: usage.clone(),
        });
        sink.finish(&ModelResponse {
            id: None,
            model: "test".into(),
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage,
        });
        sink.emit(ModelEvent::TextDelta {
            delta: "late".into(),
        });
        events.shutdown().await;
        let recorded = recorded.lock().unwrap();
        let counts = recorded
            .iter()
            .filter_map(|e| match e {
                AgentRawEvent::UsageUpdated { usage } => usage.total_tokens,
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(counts, vec![7]);
        assert!(
            !recorded.iter().any(
                |e| matches!(e,AgentRawEvent::ContentChunk {delta,..} if delta.as_ref()=="late")
            )
        );
    }

    #[tokio::test]
    async fn abort_closes_partial_streams_and_retains_the_latest_reported_usage() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let target = recorded.clone();
        let events = AgentEventEmitter::new(Arc::new(
            move |event: &super::super::AgentRawEventEnvelope| {
                target.lock().unwrap().push(event.event.clone());
            },
        ));
        let sink = RunModelEventSink::new(events.clone());
        sink.emit(ModelEvent::TextDelta {
            delta: "partial".into(),
        });
        sink.emit(ModelEvent::ToolCallDelta {
            index: 1,
            id_delta: Some("c".into()),
            name_delta: None,
            arguments_delta: None,
        });
        sink.emit(ModelEvent::Usage {
            usage: ModelUsage {
                total_tokens: Some(5),
                ..Default::default()
            },
        });
        sink.abort("cancel");
        sink.abort("second cancel");
        events.shutdown().await;
        let recorded = recorded.lock().unwrap();
        assert!(recorded.iter().any(|e| matches!(
            e,
            AgentRawEvent::ContentEnded {
                outcome: StreamEnd::Aborted { .. },
                ..
            }
        )));
        assert!(recorded.iter().any(|e| matches!(
            e,
            AgentRawEvent::ToolCallEnded {
                outcome: StreamEnd::Aborted { .. },
                tool_call: None,
                ..
            }
        )));
        assert_eq!(recorded.iter().filter(|e|matches!(e,AgentRawEvent::UsageUpdated {usage} if usage.total_tokens==Some(5))).count(),1);
    }
}
