use super::{AgentEventEmitter, AgentRawEvent, StreamEnd, ToolCallKey};
use crate::model::{ModelEvent, ModelEventSink, ModelResponse, ModelUsage};
use std::{collections::BTreeSet, sync::Mutex, time::Duration};

#[derive(Default)]
struct Progress {
    text: bool,
    reasoning: bool,
    tools: BTreeSet<u32>,
    usage: Option<ModelUsage>,
    closed: bool,
}

/// Adapts provider events to the ordered run observer. The response is authoritative for usage.
pub(crate) struct RunModelEventSink {
    events: AgentEventEmitter,
    progress: Mutex<Progress>,
}
impl RunModelEventSink {
    pub(crate) fn new(events: AgentEventEmitter) -> Self {
        Self {
            events,
            progress: Mutex::new(Progress::default()),
        }
    }

    fn close_streams(&self, state: &mut Progress, outcome: StreamEnd) {
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
        let mut state = self.progress.lock().expect("model event state poisoned");
        if state.closed {
            return;
        }
        self.close_streams(&mut state, StreamEnd::Completed);
        self.events.emit(AgentRawEvent::UsageUpdated {
            usage: response.usage.clone().into(),
        });
        self.events.emit(AgentRawEvent::ModelResponseCompleted {
            finish_reason: Some(response.stop_reason.clone()),
        });
    }

    pub(crate) fn abort(&self, reason: &str) {
        let mut state = self.progress.lock().expect("model event state poisoned");
        if state.closed {
            return;
        }
        self.close_streams(
            &mut state,
            StreamEnd::Aborted {
                reason: reason.into(),
            },
        );
        self.events.emit(AgentRawEvent::UsageUpdated {
            usage: state.usage.take().unwrap_or_default().into(),
        });
    }
}

impl ModelEventSink for RunModelEventSink {
    fn emit(&self, event: ModelEvent) {
        let mut state = self.progress.lock().expect("model event state poisoned");
        if state.closed {
            return;
        }
        match event {
            ModelEvent::Started => self.events.emit(AgentRawEvent::ModelResponseStarted),
            ModelEvent::TextDelta { delta } => {
                if !state.text {
                    state.text = true;
                    self.events
                        .emit(AgentRawEvent::ContentStarted { response_index: 0 });
                }
                self.events.emit(AgentRawEvent::ContentChunk {
                    response_index: 0,
                    delta: delta.into(),
                });
            }
            ModelEvent::ReasoningDelta { delta } => {
                if !state.reasoning {
                    state.reasoning = true;
                    self.events
                        .emit(AgentRawEvent::ReasoningStarted { response_index: 0 });
                }
                self.events.emit(AgentRawEvent::ReasoningChunk {
                    response_index: 0,
                    delta: delta.into(),
                });
            }
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
                self.events.emit(AgentRawEvent::ToolCallChunk {
                    key,
                    id_delta: id_delta.map(Into::into),
                    name_delta: name_delta.map(Into::into),
                    arguments_delta: arguments_delta.map(Into::into),
                });
            }
            ModelEvent::ToolCallCompleted { index, call } => {
                if index > i32::MAX as u32 {
                    return;
                }
                let key = ToolCallKey {
                    response_index: 0,
                    tool_index: index as i32,
                };
                if !state.tools.remove(&index) {
                    self.events.emit(AgentRawEvent::ToolCallStarted { key });
                }
                self.events.emit(AgentRawEvent::ToolCallEnded {
                    key,
                    outcome: StreamEnd::Completed,
                    tool_call: Some(call.into()),
                });
            }
            ModelEvent::Usage { usage } => state.usage = Some(usage),
            ModelEvent::Retry {
                attempt,
                delay_ms,
                reason,
            } => self.events.emit(AgentRawEvent::ModelRetryScheduled {
                attempt,
                delay: Duration::from_millis(delay_ms),
                reason: reason.into(),
            }),
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
