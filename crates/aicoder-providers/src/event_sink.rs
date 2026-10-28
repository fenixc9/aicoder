use aicoder_core::events::{AgentEventEmitter, AgentRawEvent};

/// Internal transport observation boundary; normalized model calls do not require a run thread.
pub(crate) trait ProviderEventSink: Send + Sync {
    fn emit(&self, event: AgentRawEvent);
}

impl ProviderEventSink for AgentEventEmitter {
    fn emit(&self, event: AgentRawEvent) {
        AgentEventEmitter::emit(self, event);
    }
}
